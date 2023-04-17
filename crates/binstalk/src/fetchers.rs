use std::{path::Path, sync::Arc};

use compact_str::CompactString;
pub use gh_crate_meta::*;
pub use quickinstall::*;
use tokio::sync::OnceCell;
use tracing::{debug, instrument};
use url::Url;

use crate::{
    errors::BinstallError,
    helpers::{
        download::ExtractedFiles, gh_api_client::GhApiClient, remote::Client,
        tasks::AutoAbortJoinHandle,
    },
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

pub(crate) mod gh_crate_meta;
pub(crate) mod quickinstall;

use gh_crate_meta::hosting::RepositoryHost;

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// Create a new fetcher from some data
    #[allow(clippy::new_ret_no_self)]
    fn new(
        client: Client,
        gh_api_client: GhApiClient,
        data: Arc<Data>,
        target_data: Arc<TargetData>,
    ) -> Arc<dyn Fetcher>
    where
        Self: Sized;

    /// Fetch a package and extract
    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, BinstallError>;

    /// Find the package, if it is available for download
    ///
    /// This may look for multiple remote targets, but must write (using some form of interior
    /// mutability) the best one to the implementing struct in some way so `fetch_and_extract` can
    /// proceed without additional work.
    ///
    /// Must return `true` if a package is available, `false` if none is, and reserve errors to
    /// fatal conditions only.
    fn find(self: Arc<Self>) -> AutoAbortJoinHandle<Result<bool, BinstallError>>;

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
}

#[derive(Clone, Debug)]
struct RepoInfo {
    repo: Url,
    repository_host: RepositoryHost,
    subcrate_prefix: Option<String>,
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
    async fn get_repo_info(&self, client: &Client) -> Result<&Option<RepoInfo>, BinstallError> {
        self.repo_info
            .get_or_try_init(move || {
                Box::pin(async move {
                    if let Some(repo) = self.repo.as_deref() {
                        let mut repo = client.get_redirected_final_url(Url::parse(repo)?).await?;
                        let repository_host = RepositoryHost::guess_git_hosting_services(&repo);

                        let repo_info = RepoInfo {
                            subcrate_prefix: RepoInfo::detect_subcrate_prefix(
                                &mut repo,
                                repository_host,
                            ),
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
    /// If `repo` contains a subcrate, then extracts that and return it as
    /// `{subcrate}%2F` and removes that subcrate path from `repo` to match
    /// `scheme:/{repo_owner}/{repo_name}`
    fn detect_subcrate_prefix(repo: &mut Url, repository_host: RepositoryHost) -> Option<String> {
        if repository_host != RepositoryHost::GitHub {
            return None;
        }

        let mut path_segments = repo.path_segments()?;

        let _repo_owner = path_segments.next()?;
        let _repo_name = path_segments.next()?;

        // Skip path segment "tree" and "main"
        if (path_segments.next()?, path_segments.next()?) != ("tree", "main") {
            return None;
        }

        let subcrate_name = path_segments.next()?;

        if path_segments.next().is_some() {
            // A subcrate url should not contain anything more.
            None
        } else {
            // %2F is escaped form of '/'
            let subcrate_prefix = format!("{subcrate_name}%2F");

            // Pop subcrate path to match regular repo style:
            //
            // scheme:/{repo_owner}/{repo_name}
            //
            // path_segments() succeeds, so path_segments_mut()
            // must also succeeds.
            let mut paths = repo.path_segments_mut().unwrap();

            paths.pop(); // pop subcrate_name
            paths.pop(); // pop "main"
            paths.pop(); // pop "tree"

            Some(subcrate_prefix)
        }
    }
}

/// Target specific data required to fetch a package
#[derive(Clone, Debug)]
pub struct TargetData {
    pub target: String,
    pub meta: PkgMeta,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_detect_subcrate_prefix() {
        let mut repo =
            Url::parse("https://github.com/RustSec/rustsec/tree/main/cargo-audit").unwrap();

        let repository_host = RepositoryHost::guess_git_hosting_services(&repo);
        assert_eq!(repository_host, RepositoryHost::GitHub);

        let subcrate_prefix = RepoInfo::detect_subcrate_prefix(&mut repo, repository_host).unwrap();
        assert_eq!(subcrate_prefix, "cargo-audit%2F");

        assert_eq!(
            repo,
            Url::parse("https://github.com/RustSec/rustsec").unwrap()
        );
    }
}
