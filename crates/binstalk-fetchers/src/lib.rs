#![cfg_attr(docsrs, feature(doc_auto_cfg))]

use std::{path::Path, sync::Arc};

use binstalk_downloader::{
    download::DownloadError, gh_api_client::GhApiError, remote::Error as RemoteError,
};
use binstalk_types::cargo_toml_binstall::SigningAlgorithm;
use thiserror::Error as ThisError;
use tokio::sync::OnceCell;
pub use url::ParseError as UrlParseError;

mod gh_crate_meta;
pub use gh_crate_meta::*;

#[cfg(feature = "quickinstall")]
mod quickinstall;
#[cfg(feature = "quickinstall")]
pub use quickinstall::*;

#[cfg(feature = "dist-manifest")]
mod dist_manifest_fetcher;
#[cfg(feature = "dist-manifest")]
pub use dist_manifest_fetcher::GhDistManifest;

mod common;
use common::*;

mod signing;
use signing::*;

mod futures_resolver;

use gh_crate_meta::hosting::RepositoryHost;

#[derive(Debug, ThisError)]
#[error("Invalid pkg-url {pkg_url} for {crate_name}@{version} on {target}: {reason}")]
pub struct InvalidPkgFmtError {
    pub crate_name: CompactString,
    pub version: CompactString,
    pub target: CompactString,
    pub pkg_url: Box<str>,
    pub reason: &'static &'static str,
}

#[derive(Debug, ThisError, miette::Diagnostic)]
#[non_exhaustive]
#[cfg_attr(feature = "miette", derive(miette::Diagnostic))]
pub enum FetchError {
    #[error(transparent)]
    Download(#[from] DownloadError),

    #[error("Failed to parse template: {0}")]
    #[diagnostic(transparent)]
    TemplateParse(#[from] leon::ParseError),

    #[error("Failed to render template: {0}")]
    #[diagnostic(transparent)]
    TemplateRender(#[from] leon::RenderError),

    #[error("Failed to render template: {0}")]
    GhApi(#[from] GhApiError),

    #[error(transparent)]
    InvalidPkgFmt(Box<InvalidPkgFmtError>),

    #[error("Failed to parse url: {0}")]
    UrlParse(#[from] UrlParseError),

    #[error("Signing algorithm not supported: {0:?}")]
    UnsupportedSigningAlgorithm(SigningAlgorithm),

    #[error("No signature present")]
    MissingSignature,

    #[error("Failed to verify signature")]
    InvalidSignature,

    #[cfg(feature = "dist-manifest")]
    #[error("Invalid dist manifest: {0}")]
    InvalidDistManifest(Cow<'static, str>),
}

impl From<RemoteError> for FetchError {
    fn from(e: RemoteError) -> Self {
        DownloadError::from(e).into()
    }
}

impl From<InvalidPkgFmtError> for FetchError {
    fn from(e: InvalidPkgFmtError) -> Self {
        Self::InvalidPkgFmt(Box::new(e))
    }
}

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// Create a new fetcher from some data
    #[allow(clippy::new_ret_no_self)]
    fn new(
        client: Client,
        gh_api_client: GhApiClient,
        cacher: HTTPCacher,
        data: Arc<Data>,
        target_data: Arc<TargetDataErased>,
        signature_policy: SignaturePolicy,
    ) -> Arc<dyn Fetcher>
    where
        Self: Sized;

    /// Fetch a package and extract
    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, FetchError>;

    /// Find the package, if it is available for download
    ///
    /// This may look for multiple remote targets, but must write (using some form of interior
    /// mutability) the best one to the implementing struct in some way so `fetch_and_extract` can
    /// proceed without additional work.
    ///
    /// Must return `true` if a package is available, `false` if none is, and reserve errors to
    /// fatal conditions only.
    fn find(self: Arc<Self>) -> JoinHandle<Result<bool, FetchError>>;

    /// Report to upstream that cargo-binstall tries to use this fetcher.
    /// Currently it is only overriden by [`quickinstall::QuickInstall`].
    fn report_to_upstream(self: Arc<Self>) {}

    /// Return the package format
    fn pkg_fmt(&self) -> PkgFmt;

    /// Return finalized target meta.
    fn target_meta(&self) -> PkgMeta;

    /// A short human-readable name or descriptor for the package source
    fn source_name(&self) -> CompactString;

    /// A short human-readable name, must contains only characters
    /// and numbers and it also must be unique.
    ///
    /// It is used to create a temporary dir where it is used for
    /// [`Fetcher::fetch_and_extract`].
    fn fetcher_name(&self) -> &'static str;

    /// Should return true if the remote is from a third-party source
    fn is_third_party(&self) -> bool;

    /// Return the target for this fetcher
    fn target(&self) -> &str;

    fn target_data(&self) -> &Arc<TargetDataErased>;
}

#[derive(Clone, Debug)]
struct RepoInfo {
    repo: Url,
    repository_host: RepositoryHost,
    subcrate: Option<CompactString>,
}

/// What to do about package signatures
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignaturePolicy {
    /// Don't process any signing information at all
    Ignore,

    /// Verify and fail if a signature is found, but pass a signature-less package
    IfPresent,

    /// Require signatures to be present (and valid)
    Require,
}

/// Data required to fetch a package
#[derive(Clone, Debug)]
pub struct Data {
    name: CompactString,
    version: CompactString,
    repo: Option<String>,
    repo_info: OnceCell<Option<RepoInfo>>,
}

impl Data {
    pub fn new(name: CompactString, version: CompactString, repo: Option<String>) -> Self {
        Self {
            name,
            version,
            repo,
            repo_info: OnceCell::new(),
        }
    }

    #[instrument(level = "debug")]
    async fn get_repo_info(&self, client: &Client) -> Result<&Option<RepoInfo>, FetchError> {
        self.repo_info
            .get_or_try_init(move || {
                Box::pin(async move {
                    if let Some(repo) = self.repo.as_deref() {
                        let mut repo = client.get_redirected_final_url(Url::parse(repo)?).await?;
                        let repository_host = RepositoryHost::guess_git_hosting_services(&repo);

                        let repo_info = RepoInfo {
                            subcrate: RepoInfo::detect_subcrate(&mut repo, repository_host),
                            repo,
                            repository_host,
                        };

                        debug!("Resolved repo_info = {repo_info:#?}");

                        Ok(Some(repo_info))
                    } else {
                        Ok(None)
                    }
                })
            })
            .await
    }
}

impl RepoInfo {
    /// If `repo` contains a subcrate, then extracts and returns it.
    /// It will also remove that subcrate path from `repo` to match
    /// `scheme:/{repo_owner}/{repo_name}`
    fn detect_subcrate(repo: &mut Url, repository_host: RepositoryHost) -> Option<CompactString> {
        match repository_host {
            RepositoryHost::GitHub => Self::detect_subcrate_common(repo, &["tree"]),
            RepositoryHost::GitLab => Self::detect_subcrate_common(repo, &["-", "blob"]),
            _ => None,
        }
    }

    fn detect_subcrate_common(repo: &mut Url, seps: &[&str]) -> Option<CompactString> {
        let mut path_segments = repo.path_segments()?;

        let _repo_owner = path_segments.next()?;
        let _repo_name = path_segments.next()?;

        // Skip separators
        for sep in seps.iter().copied() {
            if path_segments.next()? != sep {
                return None;
            }
        }

        // Skip branch name
        let _branch_name = path_segments.next()?;

        let (subcrate, is_crate_present) = match path_segments.next()? {
            // subcrate url is of path /crates/$subcrate_name, e.g. wasm-bindgen-cli
            "crates" => (path_segments.next()?, true),
            // subcrate url is of path $subcrate_name, e.g. cargo-audit
            subcrate => (subcrate, false),
        };

        if path_segments.next().is_some() {
            // A subcrate url should not contain anything more.
            None
        } else {
            let subcrate = subcrate.into();

            // Pop subcrate path to match regular repo style:
            //
            // scheme:/{addr}/{repo_owner}/{repo_name}
            //
            // path_segments() succeeds, so path_segments_mut()
            // must also succeeds.
            let mut paths = repo.path_segments_mut().unwrap();

            paths.pop(); // pop subcrate
            if is_crate_present {
                paths.pop(); // pop crate
            }
            paths.pop(); // pop branch name
            seps.iter().for_each(|_| {
                paths.pop();
            }); // pop separators

            Some(subcrate)
        }
    }
}

/// Target specific data required to fetch a package
#[derive(Clone, Debug)]
pub struct TargetData<T: leon::Values + ?Sized> {
    pub target: String,
    pub meta: PkgMeta,
    /// More target related info, it's recommend to provide the following keys:
    ///  - target_family,
    ///  - target_arch
    ///  - target_libc
    ///  - target_vendor
    pub target_related_info: T,
}

pub type TargetDataErased = TargetData<dyn leon::Values + Send + Sync + 'static>;

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_detect_subcrate_github() {
        // cargo-audit
        let urls = [
            "https://github.com/RustSec/rustsec/tree/main/cargo-audit",
            "https://github.com/RustSec/rustsec/tree/master/cargo-audit",
        ];
        for url in urls {
            let mut repo = Url::parse(url).unwrap();

            let repository_host = RepositoryHost::guess_git_hosting_services(&repo);
            assert_eq!(repository_host, RepositoryHost::GitHub);

            let subcrate_prefix = RepoInfo::detect_subcrate(&mut repo, repository_host).unwrap();
            assert_eq!(subcrate_prefix, "cargo-audit");

            assert_eq!(
                repo,
                Url::parse("https://github.com/RustSec/rustsec").unwrap()
            );
        }

        // wasm-bindgen-cli
        let urls = [
            "https://github.com/rustwasm/wasm-bindgen/tree/main/crates/cli",
            "https://github.com/rustwasm/wasm-bindgen/tree/master/crates/cli",
        ];
        for url in urls {
            let mut repo = Url::parse(url).unwrap();

            let repository_host = RepositoryHost::guess_git_hosting_services(&repo);
            assert_eq!(repository_host, RepositoryHost::GitHub);

            let subcrate_prefix = RepoInfo::detect_subcrate(&mut repo, repository_host).unwrap();
            assert_eq!(subcrate_prefix, "cli");

            assert_eq!(
                repo,
                Url::parse("https://github.com/rustwasm/wasm-bindgen").unwrap()
            );
        }
    }

    #[test]
    fn test_detect_subcrate_gitlab() {
        let urls = [
            "https://gitlab.kitware.com/NobodyXu/hello/-/blob/main/cargo-binstall",
            "https://gitlab.kitware.com/NobodyXu/hello/-/blob/master/cargo-binstall",
        ];
        for url in urls {
            let mut repo = Url::parse(url).unwrap();

            let repository_host = RepositoryHost::guess_git_hosting_services(&repo);
            assert_eq!(repository_host, RepositoryHost::GitLab);

            let subcrate_prefix = RepoInfo::detect_subcrate(&mut repo, repository_host).unwrap();
            assert_eq!(subcrate_prefix, "cargo-binstall");

            assert_eq!(
                repo,
                Url::parse("https://gitlab.kitware.com/NobodyXu/hello").unwrap()
            );
        }
    }
}
