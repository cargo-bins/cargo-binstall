#![cfg_attr(docsrs, feature(doc_cfg))]

use std::{borrow::Cow, path::Path, sync::Arc, time::Duration};

use binstalk_downloader::{download::DownloadError, remote::Error as RemoteError};
use binstalk_git_repo_api::gh_api_client::{GhApiError, GhRepo, RepoInfo as GhRepoInfo};
use binstalk_types::{
    cargo_toml_binstall::{SigningAlgorithm, Strategy},
    crate_info::PkgUrlHint,
};
use thiserror::Error as ThisError;
use tokio::{sync::OnceCell, task::JoinError, time::sleep};
pub use url::ParseError as UrlParseError;

mod gh_crate_meta;
pub use gh_crate_meta::*;

#[cfg(feature = "quickinstall")]
mod quickinstall;
#[cfg(feature = "quickinstall")]
pub use quickinstall::*;

mod common;
use common::*;

mod signing;
use signing::*;

mod futures_resolver;

mod fetch_context;
pub use fetch_context::FetchContext;

mod extracted;
pub use extracted::{Extracted, Fetched};

use gh_crate_meta::hosting::RepositoryHost;

static DEFAULT_GH_API_RETRY_DURATION: Duration = Duration::from_secs(1);

/// Longest GitHub API rate-limit wait worth sleeping through.
///
/// A primary rate limit resets at most hourly, far beyond
/// `--maximum-resolution-timeout`, so sleeping through it only stalls the
/// resolution until it is timed out. Short waits, e.g. a secondary rate
/// limit's `Retry-After`, are still honoured.
const MAX_GH_API_RATE_LIMIT_WAIT: Duration = Duration::from_secs(5);

/// How long to sleep before retrying a rate-limited GitHub API call, or `None`
/// if the wait is too long and the caller should give up on the API instead.
fn gh_api_rate_limit_wait(retry_after: Option<Duration>) -> Option<Duration> {
    let wait = retry_after.unwrap_or(DEFAULT_GH_API_RETRY_DURATION);
    (wait <= MAX_GH_API_RATE_LIMIT_WAIT).then_some(wait)
}

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

    #[error("Failed to wait for task: {0}")]
    TaskJoinError(#[from] JoinError),
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
    /// Create a new fetcher from some data.
    ///
    /// `telemetry`: whether the fetcher may report to its upstream how
    /// finding and installing the package went (only QuickInstall does).
    #[allow(clippy::new_ret_no_self)]
    fn new(
        client: Client,
        gh_api_client: GhApiClient,
        data: Arc<Data>,
        target_data: Arc<TargetDataErased>,
        signature_policy: SignaturePolicy,
        telemetry: bool,
    ) -> Arc<dyn Fetcher>
    where
        Self: Sized;

    /// Find the package, if it is available for download.
    ///
    /// This may look for multiple remote targets, and returns the best one,
    /// which [`Found::fetch_and_extract`] downloads without additional work.
    ///
    /// Must return `None` if no package is available, and reserve errors to
    /// fatal conditions only.
    async fn find(self: Arc<Self>) -> Result<Option<Box<dyn Found>>, FetchError>;

    /// A short human-readable name, must contains only characters
    /// and numbers and it also must be unique.
    ///
    /// It is used to create a temporary dir where it is used for
    /// [`Found::fetch_and_extract`].
    fn fetcher_name(&self) -> &'static str;

    /// The strategy used by this fetcher
    fn strategy(&self) -> Strategy;
}

/// A package [`Fetcher::find`] found.
#[async_trait::async_trait]
pub trait Found: Send + Sync {
    /// Fetch the package and extract it to `dst`, unless it was extracted
    /// for another crate already, see [`FetchContext`]: then that one is
    /// returned, which is somewhere else.
    ///
    /// Returns one package, or one per binary when `pkg-url` contains
    /// `{ bin }`, each in its own dir under `dst`.
    async fn fetch_and_extract(&mut self, dst: &Path) -> Result<Vec<Fetched>, FetchError>;

    /// Return the package format
    fn pkg_fmt(&self) -> PkgFmt;

    /// Return finalized target meta.
    fn target_meta(&self) -> PkgMeta;

    /// A short human-readable name or descriptor for the package source
    fn source_name(&self) -> CompactString;

    /// Should return true if the remote is from a third-party source
    fn is_third_party(&self) -> bool;

    /// Return the target of the package
    fn target(&self) -> &str;

    fn target_data(&self) -> &Arc<TargetDataErased>;

    /// The name `{ target-arch }` was rendered as in the package's URL, if
    /// [`Fetcher::find`] picked an alias rather than the name from
    /// [`TargetData::target_related_info`].
    ///
    /// Templates rendered for the package, such as `bin-dir`, should use it
    /// too, see [`TargetArchOverride`].
    fn resolved_target_arch(&self) -> Option<&str> {
        None
    }

    /// How [`Fetcher::find`] found the package, worth trying first next time,
    /// see [`Data::with_pkg_url_hint`].
    fn pkg_url_hint(&self) -> Option<PkgUrlHint> {
        None
    }
}

/// [`leon::Values`] that gives `target-arch` from
/// [`Found::resolved_target_arch`] when it is set, and everything else
/// from `values`.
pub struct TargetArchOverride<'a> {
    pub values: &'a dyn leon::Values,
    pub target_arch: Option<&'a str>,
}

impl leon::Values for TargetArchOverride<'_> {
    fn get_value<'s>(&'s self, key: &str) -> Option<Cow<'s, str>> {
        match (key, self.target_arch) {
            ("target-arch", Some(target_arch)) => Some(Cow::Borrowed(target_arch)),
            _ => self.values.get_value(key),
        }
    }
}

#[derive(Clone, Debug)]
struct RepoInfo {
    repo: Url,
    repository_host: RepositoryHost,
    subcrate: Option<CompactString>,
    is_private: bool,
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
#[derive(Debug)]
pub struct Data {
    name: CompactString,
    version: CompactString,
    repo: Option<String>,
    /// Shared with the other crates being resolved.
    context: Arc<FetchContext>,
    /// The crate's own view of what `context` keeps about its repository,
    /// with its own subcrate, see [`Data::get_repo_info`].
    repo_info: Arc<OnceCell<Option<RepoInfo>>>,
    /// The target the hint was found for, and the hint.
    pkg_url_hint: Option<(CompactString, PkgUrlHint)>,
    /// See [`Data::with_bins`].
    bins: Vec<BinInfo>,
}

/// A binary to install, see [`Data::with_bins`].
#[derive(Clone, Debug)]
pub struct BinInfo {
    pub name: CompactString,
    /// Whether the binary may be missing, e.g. if it has `required-features`.
    pub optional: bool,
}

impl Data {
    /// `context` is shared by all crates resolved together, so that what
    /// is learnt about a repository is asked only once.
    pub fn new(
        context: Arc<FetchContext>,
        name: CompactString,
        version: CompactString,
        repo: Option<String>,
    ) -> Self {
        Self {
            name,
            version,
            repo,
            context,
            repo_info: Arc::default(),
            pkg_url_hint: None,
            bins: Vec::new(),
        }
    }

    /// The same crate, looked up by another name, e.g. its binary's.
    ///
    /// What is known about the repository is shared, so it is asked only
    /// once.
    pub fn with_name(&self, name: CompactString) -> Self {
        Self {
            name,
            version: self.version.clone(),
            repo: self.repo.clone(),
            context: Arc::clone(&self.context),
            repo_info: Arc::clone(&self.repo_info),
            pkg_url_hint: None,
            bins: self.bins.clone(),
        }
    }

    /// The binaries to install.
    ///
    /// Fetchers that find one package per binary, like [`GhCrateMeta`] with
    /// `{ bin }` in `pkg-url`, need them to know which packages to look for,
    /// and find nothing without them.
    pub fn with_bins(mut self, bins: Vec<BinInfo>) -> Self {
        self.bins = bins;
        self
    }

    /// Have fetchers try how a previous version was found on `target` first,
    /// before probing everything else.
    ///
    /// It is ignored unless it was found with this [`Data`]'s name.
    pub fn with_pkg_url_hint(mut self, target: CompactString, hint: PkgUrlHint) -> Self {
        if hint.name == self.name {
            self.pkg_url_hint = Some((target, hint));
        }
        self
    }

    fn pkg_url_hint(&self, target: &str) -> Option<&PkgUrlHint> {
        self.pkg_url_hint
            .as_ref()
            .filter(|(hint_target, _)| hint_target == target)
            .map(|(_, hint)| hint)
    }

    /// What is known about the crate's repository.
    ///
    /// It is kept in `context` by the repository without the path of the
    /// subcrate, so that crates in subdirectories of the same repository,
    /// e.g. nushell's plugins, ask about it once.
    #[instrument(skip(client))]
    async fn get_repo_info(&self, client: &GhApiClient) -> Result<Option<&RepoInfo>, FetchError> {
        self.repo_info
            .get_or_try_init(|| async {
                let Some(repo) = self.repo.as_deref() else {
                    return Ok(None);
                };
                let (repo, subcrate) = RepoInfo::split_subcrate(repo);
                let info = self.context.repo_info(&repo);
                Ok(RepoInfo::get(&info, Some(&repo), client)
                    .await?
                    .map(|info| RepoInfo {
                        subcrate: subcrate.or_else(|| info.subcrate.clone()),
                        ..info.clone()
                    }))
            })
            .await
            .map(Option::as_ref)
    }
}

/// Whether `url`'s server answers `HEAD` like `GET`, so a 404 to the `HEAD`
/// needs no `GET` to confirm it.
///
/// github.com does: a private repository's page is 404 to both.
fn trusts_head(url: &Url) -> bool {
    is_github_com(url)
}

/// Whether `url` is on github.com itself.
///
/// The exact host match is deliberate, like `GhRepo::try_extract_from_url`
/// and unlike `RepositoryHost::GitHub`, which also takes GitHub Enterprise
/// hosts: they may not behave like github.com.
fn is_github_com(url: &Url) -> bool {
    url.domain() == Some("github.com")
}

/// Drop a trailing `.git` from a github.com repository url.
///
/// github.com answers `X.git` with a redirect to `X`, and a private `X.git`
/// 404s like `X`, so asking for `X` right away saves a request either way.
fn strip_github_dot_git(url: &mut Url) {
    if !is_github_com(url) {
        return;
    }
    let Some(name) = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(|name| name.strip_suffix(".git"))
        .map(CompactString::from)
    else {
        return;
    };
    // github.com repository names are `[A-Za-z0-9._-]`: nothing to escape.
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop().push(&name);
    }
}

/// Where `url` redirects to.
fn redirected_final_url(
    client: &GhApiClient,
    url: Url,
) -> impl Future<Output = Result<Url, RemoteError>> + '_ {
    let get_after_404 = !trusts_head(&url);
    client
        .remote_client()
        .get_redirected_final_url(url, get_after_404)
}

impl RepoInfo {
    /// What is known about the repository `repo`, kept in `cell`, `None` if
    /// there is none.
    async fn get<'a>(
        cell: &'a OnceCell<Option<RepoInfo>>,
        repo: Option<&str>,
        client: &GhApiClient,
    ) -> Result<Option<&'a RepoInfo>, FetchError> {
        async fn gh_get_repo_info(
            client: &GhApiClient,
            gh_repo: &GhRepo,
        ) -> Result<GhRepoInfo, GhApiError> {
            loop {
                match client.get_repo_info(gh_repo).await {
                    Ok(Some(gh_repo_info)) => break Ok(gh_repo_info),
                    Ok(None) => break Err(GhApiError::NotFound),
                    Err(GhApiError::RateLimit { retry_after }) => {
                        match gh_api_rate_limit_wait(retry_after) {
                            Some(wait) => sleep(wait).await,
                            None => break Err(GhApiError::RateLimit { retry_after }),
                        }
                    }
                    Err(err) => break Err(err),
                }
            }
        }

        async fn get_repo_info_inner(
            repo: &str,
            client: &GhApiClient,
        ) -> Result<RepoInfo, FetchError> {
            let mut repo = Url::parse(repo)?;
            strip_github_dot_git(&mut repo);
            let redirected = redirected_final_url(client, repo.clone()).await;
            // github.com shows a private repository only to its members
            // signed in with a browser, so one whose page loads is public:
            // no need to spend the API on it.
            let has_public_page = redirected.is_ok();
            let mut repo = redirected.unwrap_or(repo);
            let repository_host = RepositoryHost::guess_git_hosting_services(&repo);

            let subcrate = RepoInfo::detect_subcrate(&mut repo, repository_host);

            if let Some(repo) = repo
                .as_str()
                .strip_suffix(".git")
                .and_then(|s| Url::parse(s).ok())
            {
                let repository_host = RepositoryHost::guess_git_hosting_services(&repo);
                match GhRepo::try_extract_from_url(&repo) {
                    Some(gh_repo) if client.has_gh_token() && !has_public_page => {
                        if let Ok(gh_repo_info) = gh_get_repo_info(client, &gh_repo).await {
                            return Ok(RepoInfo {
                                subcrate,
                                repository_host,
                                repo,
                                is_private: gh_repo_info.is_private(),
                            });
                        }
                    }
                    _ => {
                        if let Ok(repo) = redirected_final_url(client, repo).await {
                            return Ok(RepoInfo {
                                subcrate,
                                repository_host: RepositoryHost::guess_git_hosting_services(&repo),
                                repo,
                                is_private: false,
                            });
                        }
                    }
                }
            }

            Ok(RepoInfo {
                is_private: match GhRepo::try_extract_from_url(&repo) {
                    Some(gh_repo) if client.has_gh_token() && !has_public_page => {
                        gh_get_repo_info(client, &gh_repo).await?.is_private()
                    }
                    _ => false,
                },
                subcrate,
                repo,
                repository_host,
            })
        }

        cell.get_or_try_init(move || {
            Box::pin(async move {
                let Some(repo) = repo else {
                    return Ok(None);
                };

                let repo_info = get_repo_info_inner(repo, client).await?;

                debug!("Resolved repo_info = {repo_info:#?}");

                Ok(Some(repo_info))
            })
        })
        .await
        .map(Option::as_ref)
    }

    /// `repo` without the path of its subcrate, and the subcrate, if its
    /// host can be told from it.
    ///
    /// Otherwise `repo` is kept as is: [`RepoInfo::get`] looks for the
    /// subcrate once it has followed the redirects, which may lead to a host
    /// it knows.
    fn split_subcrate(repo: &str) -> (Cow<'_, str>, Option<CompactString>) {
        if let Ok(mut url) = Url::parse(repo) {
            let repository_host = RepositoryHost::guess_git_hosting_services(&url);
            if let Some(subcrate) = Self::detect_subcrate(&mut url, repository_host) {
                return (Cow::Owned(url.into()), Some(subcrate));
            }
        }
        (Cow::Borrowed(repo), None)
    }

    /// If `repo` contains a subcrate, then extracts and returns it.
    /// It will also remove that subcrate path from `repo` to match
    /// `scheme:/{repo_owner}/{repo_name}`
    fn detect_subcrate(repo: &mut Url, repository_host: RepositoryHost) -> Option<CompactString> {
        match repository_host {
            RepositoryHost::GitHub => Self::detect_subcrate_common(repo, &["tree"]),
            RepositoryHost::GitLab => Self::detect_subcrate_common(repo, &["-", "blob"]),
            RepositoryHost::Codeberg => Self::detect_subcrate_common(repo, &["src", "branch"]),
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
            let Ok(mut paths) = repo.path_segments_mut() else {
                // Unreachable: path_segments() succeeded above, so the url
                // can be a base.
                return None;
            };

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
    use std::num::{NonZeroU16, NonZeroU64};

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

    #[tokio::test]
    async fn test_ignore_dot_git_for_github_repos() {
        let url_without_git = "https://github.com/cargo-bins/cargo-binstall";
        let url_with_git = format!("{}.git", url_without_git);

        let data = Data::new(
            Default::default(),
            "cargo-binstall".into(),
            "v1.2.3".into(),
            Some(url_with_git),
        );

        let gh_client = GhApiClient::new(
            Client::new(
                "user-agent",
                None,
                false,
                Default::default(),
                NonZeroU16::new(1000).unwrap(),
                NonZeroU64::new(1000).unwrap(),
                [],
            )
            .unwrap(),
            None,
        );

        let repo_info = data.get_repo_info(&gh_client).await.unwrap().unwrap();

        assert_eq!(url_without_git, repo_info.repo.as_str());
    }

    #[test]
    fn test_trusts_head() {
        for (url, trusted) in [
            ("https://github.com/cargo-bins/cargo-binstall", true),
            ("https://github.com/cargo-bins/cargo-binstall.git", true),
            // GitHub Enterprise and other hosts may still need the GET.
            ("https://github.example.com/a/b", false),
            ("https://gitlab.com/a/b", false),
            ("https://codeberg.org/a/b", false),
        ] {
            assert_eq!(trusts_head(&Url::parse(url).unwrap()), trusted, "{url}");
        }
    }

    #[test]
    fn test_strip_github_dot_git() {
        for (url, stripped) in [
            (
                "https://github.com/cargo-bins/cargo-binstall.git",
                "https://github.com/cargo-bins/cargo-binstall",
            ),
            (
                "https://github.com/cargo-bins/cargo-binstall",
                "https://github.com/cargo-bins/cargo-binstall",
            ),
            // Other hosts may need the `.git`.
            ("https://gitlab.com/a/b.git", "https://gitlab.com/a/b.git"),
            (
                "https://github.example.com/a/b.git",
                "https://github.example.com/a/b.git",
            ),
        ] {
            let mut url = Url::parse(url).unwrap();
            strip_github_dot_git(&mut url);
            assert_eq!(url.as_str(), stripped);
        }
    }

    #[test]
    fn test_split_subcrate() {
        let split = |repo| {
            let (repo, subcrate) = RepoInfo::split_subcrate(repo);
            (repo.into_owned(), subcrate.map(String::from))
        };
        let nushell = "https://github.com/nushell/nushell";
        assert_eq!(
            split("https://github.com/nushell/nushell/tree/main/crates/nu_plugin_inc"),
            (nushell.to_owned(), Some("nu_plugin_inc".to_owned()))
        );
        assert_eq!(split(nushell), (nushell.to_owned(), None));
        // A host it can't tell is kept as is, the subcrate is looked for
        // after the redirects.
        let unknown = "https://git.example.com/a/b/tree/main/crates/c";
        assert_eq!(split(unknown), (unknown.to_owned(), None));
        assert_eq!(split("not a url"), ("not a url".to_owned(), None));
    }

    /// Crates in subdirectories of one repository ask about it once, and
    /// each keeps its own subcrate.
    #[tokio::test]
    async fn test_repo_info_shared_by_subcrates() {
        let repo = "https://github.com/cargo-bins/cargo-binstall";
        let context = Arc::new(FetchContext::default());
        let data = |subcrate: &str| {
            Data::new(
                Arc::clone(&context),
                subcrate.into(),
                "1.0.0".into(),
                Some(format!("{repo}/tree/main/crates/{subcrate}")),
            )
        };
        let (bin, detect_targets, root) = (
            data("bin"),
            data("detect-targets"),
            Data::new(
                Arc::clone(&context),
                "cargo-binstall".into(),
                "1.0.0".into(),
                Some(repo.into()),
            ),
        );

        let gh_client = GhApiClient::new(
            Client::new(
                "user-agent",
                None,
                false,
                Default::default(),
                NonZeroU16::new(1000).unwrap(),
                NonZeroU64::new(1000).unwrap(),
                [],
            )
            .unwrap(),
            None,
        );
        for (data, subcrate) in [
            (&bin, Some("bin")),
            (&detect_targets, Some("detect-targets")),
            (&root, None),
        ] {
            let info = data.get_repo_info(&gh_client).await.unwrap().unwrap();
            assert_eq!(info.repo.as_str(), repo);
            assert_eq!(info.subcrate.as_deref(), subcrate);
            assert!(!info.is_private);
        }
        assert_eq!(context.repo_infos_len(), 1);
    }

    #[test]
    fn test_gh_api_rate_limit_wait() {
        assert_eq!(
            gh_api_rate_limit_wait(None),
            Some(DEFAULT_GH_API_RETRY_DURATION)
        );
        assert_eq!(
            gh_api_rate_limit_wait(Some(Duration::from_secs(2))),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            gh_api_rate_limit_wait(Some(MAX_GH_API_RATE_LIMIT_WAIT)),
            Some(MAX_GH_API_RATE_LIMIT_WAIT)
        );
        // A primary rate limit's reset, e.g. the 10 minutes `GhApiClient`
        // assumes when GraphQL reports `RATE_LIMITED`, is not worth waiting.
        assert_eq!(gh_api_rate_limit_wait(Some(Duration::from_secs(600))), None);
    }
}
