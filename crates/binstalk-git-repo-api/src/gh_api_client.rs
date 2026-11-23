use std::{
    collections::{HashMap, HashSet},
    future::Future,
    ops::Deref,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};

use binstalk_downloader::{download::Download, remote};

use crate::git_refs;
use compact_str::{CompactString, ToCompactString, format_compact};
use tokio::sync::OnceCell;
use tracing::{Level, debug, instrument, warn};
use url::Url;
use zeroize::Zeroizing;

mod common;
mod error;
mod graphql;
mod release_artifacts;
mod repo_info;
mod web;

pub(crate) use common::check_http_status_and_header;
use common::percent_decode_http_url_path;
pub use error::{GhApiContextError, GhApiError, GhGraphQLErrors};
pub use repo_info::RepoInfo;
pub use web::ReleaseAssets;

/// default retry duration if x-ratelimit-reset is not found in response header
const DEFAULT_RETRY_DURATION: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct GhRepo {
    pub owner: CompactString,
    pub repo: CompactString,
}
impl GhRepo {
    pub fn repo_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&format_compact!(
            "https://github.com/{}/{}",
            self.owner,
            self.repo
        ))
    }

    pub fn try_extract_from_url(url: &Url) -> Option<Self> {
        if url.domain() != Some("github.com") {
            return None;
        }

        let mut path_segments = url.path_segments()?;

        Some(Self {
            owner: path_segments.next()?.to_compact_string(),
            repo: path_segments.next()?.to_compact_string(),
        })
    }
}

/// The keys required to identify a github release.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct GhRelease {
    pub repo: GhRepo,
    pub tag: CompactString,
}

/// The Github Release and one of its artifact.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct GhReleaseArtifact {
    pub release: GhRelease,
    pub artifact_name: CompactString,
}

impl GhReleaseArtifact {
    /// Create [`GhReleaseArtifact`] from url.
    pub fn try_extract_from_url(url: &remote::Url) -> Option<Self> {
        if url.domain() != Some("github.com") {
            return None;
        }

        let mut path_segments = url.path_segments()?;

        let owner = path_segments.next()?;
        let repo = path_segments.next()?;

        if (path_segments.next()?, path_segments.next()?) != ("releases", "download") {
            return None;
        }

        let tag = path_segments.next()?;
        let artifact_name = path_segments.next()?;

        (path_segments.next().is_none() && url.fragment().is_none() && url.query().is_none()).then(
            || Self {
                release: GhRelease {
                    repo: GhRepo {
                        owner: percent_decode_http_url_path(owner),
                        repo: percent_decode_http_url_path(repo),
                    },
                    tag: percent_decode_http_url_path(tag),
                },
                artifact_name: percent_decode_http_url_path(artifact_name),
            },
        )
    }
}

#[derive(Debug)]
struct Map<K, V>(RwLock<HashMap<K, Arc<V>>>);

impl<K, V> Default for Map<K, V> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<K, V> Map<K, V>
where
    K: Eq + std::hash::Hash,
    V: Default,
{
    fn get(&self, k: K) -> Arc<V> {
        let optional_value = self.0.read().unwrap().deref().get(&k).cloned();
        optional_value.unwrap_or_else(|| Arc::clone(self.0.write().unwrap().entry(k).or_default()))
    }
}

#[derive(Debug)]
struct Inner {
    client: remote::Client,
    release_artifacts: Map<GhRelease, OnceCell<Option<release_artifacts::Artifacts>>>,
    release_web_assets: Map<GhRelease, OnceCell<web::AssetPage>>,
    retry_after: Mutex<Option<Instant>>,

    auth_token: Option<Zeroizing<Box<str>>>,
    is_auth_token_valid: AtomicBool,

    only_use_restful_api: AtomicBool,
}

/// Trim surrounding whitespace (e.g. a trailing `\n` from a CI secret), and drop
/// a token that still can't be sent in an HTTP header: it would make every
/// request fail to build, instead of falling back to unauthenticated requests.
fn sanitize_auth_token(token: Zeroizing<Box<str>>) -> Option<Zeroizing<Box<str>>> {
    let trimmed = token.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_graphic()) {
        warn!("Ignoring the GitHub token: it is empty or contains invalid characters");
        return None;
    }
    Some(Zeroizing::new(trimmed.into()))
}

/// Github API client for querying whether a release artifact exists.
/// Can only handle github.com for now.
#[derive(Clone, Debug)]
pub struct GhApiClient(Arc<Inner>);

impl GhApiClient {
    pub fn new(client: remote::Client, auth_token: Option<Zeroizing<Box<str>>>) -> Self {
        Self(Arc::new(Inner {
            client,
            release_artifacts: Default::default(),
            release_web_assets: Default::default(),
            retry_after: Default::default(),

            auth_token: auth_token.and_then(sanitize_auth_token),
            is_auth_token_valid: AtomicBool::new(true),

            only_use_restful_api: AtomicBool::new(false),
        }))
    }

    /// If you don't want to use GitHub GraphQL API for whatever reason, call this.
    pub fn set_only_use_restful_api(&self) {
        self.0.only_use_restful_api.store(true, Relaxed);
    }

    pub fn remote_client(&self) -> &remote::Client {
        &self.0.client
    }
}

impl GhApiClient {
    fn check_retry_after(&self) -> Result<(), GhApiError> {
        let mut guard = self.0.retry_after.lock().unwrap();

        if let Some(retry_after) = *guard {
            if retry_after.elapsed().is_zero() {
                return Err(GhApiError::RateLimit {
                    retry_after: Some(retry_after - Instant::now()),
                });
            } else {
                // Instant retry_after is already reached.
                *guard = None;
            }
        }

        Ok(())
    }

    fn get_auth_token(&self) -> Option<&str> {
        if self.0.is_auth_token_valid.load(Relaxed) {
            self.0.auth_token.as_deref().map(|s| &**s)
        } else {
            None
        }
    }

    /// [`git_refs::list_tags`] for a private repository on github.com,
    /// authenticated with the token: GitHub's git server takes it as the
    /// password of HTTP Basic authentication.
    ///
    /// Fails with [`GhApiError::Unauthorized`] without a token.
    pub async fn list_private_repo_tags(
        &self,
        GhRepo { owner, repo }: &GhRepo,
        tags: HashSet<CompactString>,
    ) -> Result<git_refs::RepoTags, GhApiError> {
        let token = self.get_auth_token().ok_or(GhApiError::Unauthorized)?;

        let mut url =
            Url::parse("https://github.com/").expect("Literal provided must be a valid url");
        url.path_segments_mut()
            .expect("https url can be a base")
            .extend([owner, repo]);

        git_refs::list_tags(&self.0.client, &url, tags, Some(("x-access-token", token))).await
    }

    pub fn has_gh_token(&self) -> bool {
        self.get_auth_token().is_some()
    }

    async fn do_fetch<T, U, GraphQLFn, RestfulFn, GraphQLFut, RestfulFut>(
        &self,
        graphql_func: GraphQLFn,
        restful_func: RestfulFn,
        data: &T,
    ) -> Result<U, GhApiError>
    where
        GraphQLFn: Fn(&remote::Client, &T, &str) -> GraphQLFut,
        RestfulFn: Fn(&remote::Client, &T, Option<&str>) -> RestfulFut,
        GraphQLFut: Future<Output = Result<U, GhApiError>> + Send + 'static,
        RestfulFut: Future<Output = Result<U, GhApiError>> + Send + 'static,
    {
        self.check_retry_after()?;

        if !self.0.only_use_restful_api.load(Relaxed)
            && let Some(auth_token) = self.get_auth_token()
        {
            match graphql_func(&self.0.client, data, auth_token).await {
                Err(GhApiError::Unauthorized) => {
                    self.0.is_auth_token_valid.store(false, Relaxed);
                }
                res => return res.map_err(|err| err.context("GraphQL API")),
            }
        }

        restful_func(&self.0.client, data, self.get_auth_token())
            .await
            .map_err(|err| err.context("Restful API"))
    }

    #[instrument(skip(self), ret(level = Level::DEBUG))]
    pub async fn get_repo_info(&self, repo: &GhRepo) -> Result<Option<RepoInfo>, GhApiError> {
        match self
            .do_fetch(
                repo_info::fetch_repo_info_graphql_api,
                repo_info::fetch_repo_info_restful_api,
                repo,
            )
            .await
        {
            Ok(repo_info) => Ok(repo_info),
            Err(GhApiError::NotFound) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct GhReleaseArtifactUrl(Url);

impl GhApiClient {
    /// Return `Ok(Some(api_artifact_url))` if exists.
    ///
    /// Caches info on all artifacts matching (repo, tag).
    ///
    /// The returned future is guaranteed to be pointer size.
    #[instrument(skip(self), ret(level = Level::DEBUG))]
    pub async fn has_release_artifact(
        &self,
        GhReleaseArtifact {
            release,
            artifact_name,
        }: GhReleaseArtifact,
    ) -> Result<Option<GhReleaseArtifactUrl>, GhApiError> {
        let once_cell = self.0.release_artifacts.get(release.clone());
        let res = once_cell
            .get_or_try_init(|| {
                Box::pin(async {
                    match self
                        .do_fetch(
                            release_artifacts::fetch_release_artifacts_graphql_api,
                            release_artifacts::fetch_release_artifacts_restful_api,
                            &release,
                        )
                        .await
                    {
                        Ok(artifacts) => Ok(Some(artifacts)),
                        Err(GhApiError::NotFound) => Ok(None),
                        Err(err) => Err(err),
                    }
                })
            })
            .await;

        match res {
            Ok(Some(artifacts)) => Ok(artifacts
                .get_artifact_url(&artifact_name)
                .map(GhReleaseArtifactUrl)),
            Ok(None) => Ok(None),
            Err(GhApiError::RateLimit { retry_after }) => {
                *self.0.retry_after.lock().unwrap() =
                    Some(Instant::now() + retry_after.unwrap_or(DEFAULT_RETRY_DURATION));

                Err(GhApiError::RateLimit { retry_after })
            }
            Err(err) => Err(err),
        }
    }

    /// Fetch the assets of the releases `tags` of `repo` with one GraphQL query,
    /// so that [`GhApiClient::has_release_artifact`] finds them cached instead
    /// of sending one query per release.
    ///
    /// Does nothing without a token, since the restful API can't batch, or if
    /// fewer than two of `tags` are not cached yet: a batch of one is the
    /// query [`GhApiClient::has_release_artifact`] sends anyway. Errors are
    /// not returned: [`GhApiClient::has_release_artifact`] then fetches each
    /// release on its own, as it would have without this.
    ///
    /// A release with more assets than fit in the batch keeps its first page,
    /// the rest is fetched from there.
    ///
    /// `tags` should not repeat a tag, a repeated one is fetched twice.
    #[instrument(skip(self, tags))]
    pub async fn prefetch_release_artifacts(
        &self,
        repo: &GhRepo,
        tags: impl IntoIterator<Item = CompactString>,
    ) {
        if self.0.only_use_restful_api.load(Relaxed) || self.check_retry_after().is_err() {
            return;
        }
        let Some(auth_token) = self.get_auth_token() else {
            return;
        };

        let cells: Vec<_> = tags
            .into_iter()
            .map(|tag| {
                let cell = self.0.release_artifacts.get(GhRelease {
                    repo: repo.clone(),
                    tag: tag.clone(),
                });
                (tag, cell)
            })
            .filter(|(_, cell)| !cell.initialized())
            .collect();

        if cells.len() < 2 {
            return;
        }

        let releases = match release_artifacts::fetch_releases_artifacts_graphql_api(
            &self.0.client,
            repo,
            cells.iter().map(|(tag, _)| tag.as_str()),
            auth_token,
        )
        .await
        {
            Ok(releases) => releases,
            Err(GhApiError::NotFound) => {
                // The repository itself does not exist.
                cells.iter().for_each(|(_, cell)| {
                    let _ = cell.set(None);
                });
                return;
            }
            Err(err) => {
                self.note_prefetch_error(&err);
                debug!("Failed to fetch the assets of the releases of {repo:?}: {err}");
                return;
            }
        };

        for ((tag, cell), release) in cells.iter().zip(releases) {
            let artifacts = match release {
                release_artifacts::BatchedRelease::Found(artifacts) => Some(artifacts),
                release_artifacts::BatchedRelease::NotFound => None,
                release_artifacts::BatchedRelease::Incomplete {
                    artifacts,
                    end_cursor,
                } => {
                    match release_artifacts::fetch_rest_of_release_artifacts(
                        &self.0.client,
                        repo,
                        tag,
                        auth_token,
                        artifacts,
                        end_cursor,
                    )
                    .await
                    {
                        Ok(artifacts) => Some(artifacts),
                        Err(err) => {
                            self.note_prefetch_error(&err);
                            debug!(
                                "Failed to fetch the rest of the assets of release {tag} of {repo:?}: {err}"
                            );
                            continue;
                        }
                    }
                }
            };

            // `set` fails only if `has_release_artifact` got there first,
            // then that value is just as good.
            let _ = cell.set(artifacts);
        }
    }

    /// Remember what an error says about the token and the rate limit, as
    /// [`GhApiClient::has_release_artifact`] would.
    fn note_prefetch_error(&self, err: &GhApiError) {
        match err {
            GhApiError::Unauthorized => self.0.is_auth_token_valid.store(false, Relaxed),
            GhApiError::RateLimit { retry_after } => {
                *self.0.retry_after.lock().unwrap() =
                    Some(Instant::now() + retry_after.unwrap_or(DEFAULT_RETRY_DURATION));
            }
            _ => (),
        }
    }

    /// Which of the asset `names` of `release` github.com lists on the
    /// release's page, without the API.
    ///
    /// The list comes from a page meant for people, so an asset missing from
    /// it may still exist, and a listed one should be checked before it is
    /// used.
    ///
    /// Only use it for a public repository, github.com answers 404 for a
    /// private one.
    ///
    /// Caches the page per release, so this costs one request per release
    /// however many names are looked up.
    #[instrument(skip(self, names))]
    pub async fn release_assets_on_web<'a>(
        &self,
        release: &GhRelease,
        names: impl IntoIterator<Item = &'a str>,
    ) -> ReleaseAssets {
        self.0
            .release_web_assets
            .get(release.clone())
            .get_or_init(|| web::fetch_release_assets(&self.0.client, release))
            .await
            .lists(names)
    }

    pub async fn download_artifact(
        &self,
        artifact_url: GhReleaseArtifactUrl,
    ) -> Result<Download<'static>, GhApiError> {
        self.check_retry_after()?;

        let Some(auth_token) = self.get_auth_token() else {
            return Err(GhApiError::Unauthorized);
        };

        let response = self
            .0
            .client
            .get(artifact_url.0)
            .header("Accept", "application/octet-stream")
            .bearer_auth(&auth_token)
            .send(false)
            .await?;

        match check_http_status_and_header(response) {
            Err(GhApiError::Unauthorized) => {
                self.0.is_auth_token_valid.store(false, Relaxed);
                Err(GhApiError::Unauthorized)
            }
            res => res.map(Download::from_response),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use compact_str::{CompactString, ToCompactString};
    use std::{env, num::NonZeroU16, time::Duration};
    use tokio::time::sleep;
    use tracing::subscriber::set_global_default;
    use tracing_subscriber::{filter::LevelFilter, fmt::fmt};

    static DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);

    #[test]
    fn test_sanitize_auth_token() {
        let sanitize =
            |s: &str| sanitize_auth_token(Zeroizing::new(s.into())).map(|t| t.to_string());
        assert_eq!(sanitize("ghp_abc123\n").as_deref(), Some("ghp_abc123"));
        assert_eq!(sanitize(" ghp_abc123\r\n").as_deref(), Some("ghp_abc123"));
        assert_eq!(sanitize("ghp_abc 123"), None);
        assert_eq!(sanitize("\n"), None);
    }

    mod cargo_binstall_v0_20_1 {
        use super::{CompactString, GhRelease, GhRepo};

        pub(super) const RELEASE: GhRelease = GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("cargo-bins"),
                repo: CompactString::const_new("cargo-binstall"),
            },
            tag: CompactString::const_new("v0.20.1"),
        };

        pub(super) const ARTIFACTS: &[&str] = &[
            "cargo-binstall-aarch64-apple-darwin.full.zip",
            "cargo-binstall-aarch64-apple-darwin.zip",
            "cargo-binstall-aarch64-pc-windows-msvc.full.zip",
            "cargo-binstall-aarch64-pc-windows-msvc.zip",
            "cargo-binstall-aarch64-unknown-linux-gnu.full.tgz",
            "cargo-binstall-aarch64-unknown-linux-gnu.tgz",
            "cargo-binstall-aarch64-unknown-linux-musl.full.tgz",
            "cargo-binstall-aarch64-unknown-linux-musl.tgz",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.full.tgz",
            "cargo-binstall-armv7-unknown-linux-gnueabihf.tgz",
            "cargo-binstall-armv7-unknown-linux-musleabihf.full.tgz",
            "cargo-binstall-armv7-unknown-linux-musleabihf.tgz",
            "cargo-binstall-universal-apple-darwin.full.zip",
            "cargo-binstall-universal-apple-darwin.zip",
            "cargo-binstall-x86_64-apple-darwin.full.zip",
            "cargo-binstall-x86_64-apple-darwin.zip",
            "cargo-binstall-x86_64-pc-windows-msvc.full.zip",
            "cargo-binstall-x86_64-pc-windows-msvc.zip",
            "cargo-binstall-x86_64-unknown-linux-gnu.full.tgz",
            "cargo-binstall-x86_64-unknown-linux-gnu.tgz",
            "cargo-binstall-x86_64-unknown-linux-musl.full.tgz",
            "cargo-binstall-x86_64-unknown-linux-musl.tgz",
        ];
    }

    mod cargo_audit_v_0_17_6 {
        use super::*;

        pub(super) const RELEASE: GhRelease = GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("rustsec"),
                repo: CompactString::const_new("rustsec"),
            },
            tag: CompactString::const_new("cargo-audit/v0.17.6"),
        };

        #[allow(unused)]
        pub(super) const ARTIFACTS: &[&str] = &[
            "cargo-audit-aarch64-unknown-linux-gnu-v0.17.6.tgz",
            "cargo-audit-armv7-unknown-linux-gnueabihf-v0.17.6.tgz",
            "cargo-audit-x86_64-apple-darwin-v0.17.6.tgz",
            "cargo-audit-x86_64-pc-windows-msvc-v0.17.6.zip",
            "cargo-audit-x86_64-unknown-linux-gnu-v0.17.6.tgz",
            "cargo-audit-x86_64-unknown-linux-musl-v0.17.6.tgz",
        ];

        #[test]
        fn extract_with_escaped_characters() {
            let release_artifact = try_extract_artifact_from_str(
"https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/cargo-audit-aarch64-unknown-linux-gnu-v0.17.6.tgz"
                ).unwrap();

            assert_eq!(
                release_artifact,
                GhReleaseArtifact {
                    release: RELEASE,
                    artifact_name: CompactString::from(
                        "cargo-audit-aarch64-unknown-linux-gnu-v0.17.6.tgz",
                    )
                }
            );
        }
    }

    #[test]
    fn gh_repo_extract_from_and_to_url() {
        [
            "https://github.com/cargo-bins/cargo-binstall",
            "https://github.com/rustsec/rustsec",
        ]
        .into_iter()
        .for_each(|url| {
            let url = Url::parse(url).unwrap();
            assert_eq!(
                GhRepo::try_extract_from_url(&url)
                    .unwrap()
                    .repo_url()
                    .unwrap(),
                url
            );
        })
    }

    fn try_extract_artifact_from_str(s: &str) -> Option<GhReleaseArtifact> {
        GhReleaseArtifact::try_extract_from_url(&url::Url::parse(s).unwrap())
    }

    fn assert_extract_gh_release_artifacts_failures(urls: &[&str]) {
        for url in urls {
            assert_eq!(try_extract_artifact_from_str(url), None);
        }
    }

    #[test]
    fn extract_gh_release_artifacts_failure() {
        use cargo_binstall_v0_20_1::*;

        let GhRelease {
            repo: GhRepo { owner, repo },
            tag,
        } = RELEASE;

        assert_extract_gh_release_artifacts_failures(&[
            "https://example.com",
            "https://github.com",
            &format!("https://github.com/{owner}"),
            &format!("https://github.com/{owner}/{repo}"),
            &format!("https://github.com/{owner}/{repo}/123e"),
            &format!("https://github.com/{owner}/{repo}/releases/21343"),
            &format!("https://github.com/{owner}/{repo}/releases/download"),
            &format!("https://github.com/{owner}/{repo}/releases/download/{tag}"),
            &format!("https://github.com/{owner}/{repo}/releases/download/{tag}/a/23"),
            &format!("https://github.com/{owner}/{repo}/releases/download/{tag}/a#a=12"),
            &format!("https://github.com/{owner}/{repo}/releases/download/{tag}/a?page=3"),
        ]);
    }

    #[test]
    fn extract_gh_release_artifacts_success() {
        use cargo_binstall_v0_20_1::*;

        let GhRelease {
            repo: GhRepo { owner, repo },
            tag,
        } = RELEASE;

        for artifact in ARTIFACTS {
            let GhReleaseArtifact {
                release,
                artifact_name,
            } = try_extract_artifact_from_str(&format!(
                "https://github.com/{owner}/{repo}/releases/download/{tag}/{artifact}"
            ))
            .unwrap();

            assert_eq!(release, RELEASE);
            assert_eq!(artifact_name, artifact);
        }
    }

    fn init_logger() {
        // Disable time, target, file, line_num, thread name/ids to make the
        // output more readable
        let subscriber = fmt()
            .without_time()
            .with_target(false)
            .with_file(false)
            .with_line_number(false)
            .with_thread_names(false)
            .with_thread_ids(false)
            .with_test_writer()
            .with_max_level(LevelFilter::DEBUG)
            .finish();

        // Setup global subscriber
        let _ = set_global_default(subscriber);
    }

    fn create_remote_client() -> remote::Client {
        remote::Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
            NonZeroU16::new(300).unwrap(),
            1.try_into().unwrap(),
            [],
        )
        .unwrap()
    }

    /// Mark this as an async fn so that you won't accidentally use it in
    /// sync context.
    fn create_client() -> Vec<GhApiClient> {
        let client = create_remote_client();

        let auth_token = match env::var("CI_UNIT_TEST_GITHUB_TOKEN") {
            Ok(auth_token) if !auth_token.is_empty() => {
                Some(zeroize::Zeroizing::new(auth_token.into_boxed_str()))
            }
            _ => None,
        };

        let gh_client = GhApiClient::new(client.clone(), auth_token.clone());
        gh_client.set_only_use_restful_api();

        let mut gh_clients = vec![gh_client];

        if auth_token.is_some() {
            gh_clients.push(GhApiClient::new(client, auth_token));
        }

        gh_clients
    }

    #[tokio::test]
    async fn rate_limited_test_get_repo_info() {
        const PUBLIC_REPOS: [GhRepo; 1] = [GhRepo {
            owner: CompactString::const_new("cargo-bins"),
            repo: CompactString::const_new("cargo-binstall"),
        }];
        const PRIVATE_REPOS: [GhRepo; 1] = [GhRepo {
            owner: CompactString::const_new("cargo-bins"),
            repo: CompactString::const_new("private-repo-for-testing"),
        }];
        const NON_EXISTENT_REPOS: [GhRepo; 1] = [GhRepo {
            owner: CompactString::const_new("cargo-bins"),
            repo: CompactString::const_new("ttt"),
        }];

        init_logger();

        let mut tests: Vec<(_, _)> = Vec::new();

        for client in create_client() {
            let spawn_get_repo_info_task = |repo| {
                let client = client.clone();
                tokio::spawn(async move {
                    loop {
                        match client.get_repo_info(&repo).await {
                            Err(GhApiError::RateLimit { retry_after }) => {
                                sleep(retry_after.unwrap_or(DEFAULT_RETRY_AFTER)).await
                            }
                            res => break res,
                        }
                    }
                })
            };

            for repo in PUBLIC_REPOS {
                tests.push((
                    Some(RepoInfo::new(repo.clone(), false)),
                    spawn_get_repo_info_task(repo),
                ));
            }

            for repo in NON_EXISTENT_REPOS {
                tests.push((None, spawn_get_repo_info_task(repo)));
            }

            if client.has_gh_token() {
                for repo in PRIVATE_REPOS {
                    tests.push((
                        Some(RepoInfo::new(repo.clone(), true)),
                        spawn_get_repo_info_task(repo),
                    ));
                }
            }
        }

        for (expected, task) in tests {
            assert_eq!(task.await.unwrap().unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn rate_limited_test_prefetch_release_artifacts() {
        init_logger();

        for client in create_client() {
            // Without a token or with only the restful API, it does nothing.
            let can_batch = client.has_gh_token() && !client.0.only_use_restful_api.load(Relaxed);

            let release = cargo_binstall_v0_20_1::RELEASE;
            let non_existent_release = GhRelease {
                repo: release.repo.clone(),
                tag: CompactString::const_new("v0.18.2"),
            };

            client
                .prefetch_release_artifacts(
                    &release.repo,
                    [release.tag.clone(), non_existent_release.tag.clone()],
                )
                .await;

            let cell = client.0.release_artifacts.get(release.clone());
            let non_existent_cell = client.0.release_artifacts.get(non_existent_release);

            if !can_batch {
                assert!(!cell.initialized());
                assert!(!non_existent_cell.initialized());
                continue;
            }

            let artifacts = cell.get().unwrap().as_ref().unwrap();
            for artifact in cargo_binstall_v0_20_1::ARTIFACTS {
                assert!(artifacts.get_artifact_url(artifact).is_some(), "{artifact}");
            }
            assert!(non_existent_cell.get().unwrap().is_none());
        }
    }

    /// Compare the GraphQL rate limit cost of a query per candidate tag with
    /// one batched query, as reported by GitHub. Prints the costs.
    #[tokio::test]
    async fn rate_limited_test_graphql_cost() {
        #[derive(Debug, serde::Deserialize)]
        struct Cost {
            #[serde(rename = "rateLimit")]
            rate_limit: RateLimit,
        }
        #[derive(Debug, serde::Deserialize)]
        struct RateLimit {
            cost: u32,
        }

        init_logger();

        let Some(auth_token) = env::var("CI_UNIT_TEST_GITHUB_TOKEN")
            .ok()
            .filter(|token| !token.is_empty())
        else {
            return;
        };
        let client = create_remote_client();

        let cost = |query: String| {
            // Ask for the cost of the query itself in the same query.
            let query = format!(
                "{} rateLimit {{ cost }} }}",
                query.trim_end().strip_suffix('}').unwrap()
            );
            let future = common::issue_graphql_query::<Cost>(&client, query, &auth_token);
            async move { future.await.unwrap().rate_limit.cost }
        };

        // The tags GhCrateMeta tries for cargo-binstall 0.20.1 on the
        // default pkg-url templates.
        let repo = cargo_binstall_v0_20_1::RELEASE.repo;
        let tags = [
            "0.20.1",
            "v0.20.1",
            "cargo-binstall-v0.20.1",
            "cargo-binstall-0.20.1",
        ]
        .map(CompactString::const_new);

        let mut per_tag = Vec::new();
        for tag in &tags {
            let release = GhRelease {
                repo: repo.clone(),
                tag: tag.clone(),
            };
            per_tag.push(cost(release_artifacts::first_release_query(&release)).await);
        }
        let batched = cost(release_artifacts::batch_query(
            &repo,
            tags.iter().map(CompactString::as_str),
        ))
        .await;

        let per_tag_total: u32 = per_tag.iter().sum();
        println!(
            "GraphQL cost for {} tags: one query per tag {per_tag:?} = {per_tag_total}, batched = {batched}",
            tags.len()
        );
        assert!(batched < per_tag_total);
    }

    #[tokio::test]
    async fn test_list_private_repo_tags_needs_token() {
        // No token: nothing is sent.
        let client = GhApiClient::new(create_remote_client(), None);
        assert!(matches!(
            client
                .list_private_repo_tags(
                    &cargo_binstall_v0_20_1::RELEASE.repo,
                    HashSet::from([CompactString::const_new("v0.20.1")])
                )
                .await,
            Err(GhApiError::Unauthorized)
        ));
    }

    /// Needs `CI_UNIT_TEST_GITHUB_TOKEN` with access to the private repository.
    #[tokio::test]
    async fn test_list_private_repo_tags() {
        init_logger();

        let Some(auth_token) = env::var("CI_UNIT_TEST_GITHUB_TOKEN")
            .ok()
            .filter(|token| !token.is_empty())
        else {
            return;
        };

        let repo = GhRepo {
            owner: CompactString::const_new("cargo-bins"),
            repo: CompactString::const_new("private-repo-for-testing"),
        };
        let tags = || HashSet::from(["0.12.0".into(), "v0.12.0".into()]);

        // Without the token, the git server asks for credentials as if the
        // repository did not exist.
        let url = Url::parse("https://github.com/cargo-bins/private-repo-for-testing").unwrap();
        let err = git_refs::list_tags(&create_remote_client(), &url, tags(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, GhApiError::Unauthorized), "{err:?}");

        let client = GhApiClient::new(
            create_remote_client(),
            Some(Zeroizing::new(auth_token.into())),
        );
        let tags = client.list_private_repo_tags(&repo, tags()).await.unwrap();
        assert_eq!(tags.exists("0.12.0"), Some(true));
        assert_eq!(tags.exists("v0.12.0"), Some(false));
    }

    #[tokio::test]
    async fn test_release_assets_on_web() {
        init_logger();

        let client = GhApiClient::new(create_remote_client(), None);
        let release = cargo_binstall_v0_20_1::RELEASE;

        let names = cargo_binstall_v0_20_1::ARTIFACTS;
        assert_eq!(
            client
                .release_assets_on_web(&release, names.iter().copied())
                .await,
            ReleaseAssets::Listed((0..names.len()).collect())
        );
        assert_eq!(
            client
                .release_assets_on_web(
                    &release,
                    ["cargo-binstall-x86_64-unknown-linux-musl.tgz.sig", "a.tgz"]
                )
                .await,
            ReleaseAssets::Listed(vec![])
        );

        // The owner and repository name are case insensitive.
        let other_case = GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("Cargo-Bins"),
                repo: CompactString::const_new("Cargo-Binstall"),
            },
            tag: release.tag.clone(),
        };
        assert_eq!(
            client
                .release_assets_on_web(
                    &other_case,
                    ["a.tgz", "cargo-binstall-x86_64-unknown-linux-musl.tgz"]
                )
                .await,
            ReleaseAssets::Listed(vec![1])
        );

        // The latest release, with `.full` builds, signatures and a key.
        let latest = GhRelease {
            repo: release.repo.clone(),
            tag: CompactString::const_new("v1.28.0"),
        };
        assert_eq!(
            client
                .release_assets_on_web(
                    &latest,
                    [
                        "cargo-binstall-x86_64-unknown-linux-musl.tgz",
                        "cargo-binstall-x86_64-unknown-linux-musl.full.tgz",
                        "cargo-binstall-x86_64-unknown-linux-musl.tgz.sig",
                        "cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
                        "cargo-binstall-universal-apple-darwin.zip",
                        "minisign.pub",
                    ]
                )
                .await,
            ReleaseAssets::Listed(vec![0, 1, 2, 4, 5])
        );

        // cargo-quickinstall: a release per crate version, a tag with `+`.
        let quickinstall = GhRepo {
            owner: CompactString::const_new("cargo-bins"),
            repo: CompactString::const_new("cargo-quickinstall"),
        };
        let with_plus = GhRelease {
            repo: quickinstall.clone(),
            tag: CompactString::const_new("cargo-c-0.10.26+cargo-0.100.0"),
        };
        assert_eq!(
            client
                .release_assets_on_web(
                    &with_plus,
                    [
                        "cargo-c-0.10.26+cargo-0.100.0-aarch64-unknown-linux-gnu.tar.gz",
                        "cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz",
                        "cargo-c-0.10.26+cargo-0.100.0-x86_64-apple-darwin.tar.gz.sig",
                    ]
                )
                .await,
            ReleaseAssets::Listed(vec![1, 2])
        );
        let ripgrep = GhRelease {
            repo: quickinstall,
            tag: CompactString::const_new("ripgrep-14.1.1"),
        };
        assert_eq!(
            client
                .release_assets_on_web(
                    &ripgrep,
                    [
                        "ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz",
                        "ripgrep-14.1.1-x86_64-unknown-linux-gnu.tgz",
                    ]
                )
                .await,
            ReleaseAssets::Listed(vec![0])
        );

        // A tag without a release.
        let tag_only = GhRelease {
            repo: release.repo.clone(),
            tag: CompactString::const_new("detect-targets-v0.1.0"),
        };
        assert_eq!(
            client.release_assets_on_web(&tag_only, ["a.tgz"]).await,
            ReleaseAssets::NoAssets
        );

        // A private repository is not found without the user's cookies.
        let private = GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("cargo-bins"),
                repo: CompactString::const_new("private-repo-for-testing"),
            },
            tag: CompactString::const_new("0.12.0"),
        };
        assert_eq!(
            client.release_assets_on_web(&private, ["a.tgz"]).await,
            ReleaseAssets::NotFound
        );

        let release = GhRelease {
            repo: release.repo,
            tag: CompactString::const_new("v0.18.2"),
        };
        assert_eq!(
            client.release_assets_on_web(&release, ["a.tgz"]).await,
            ReleaseAssets::NotFound
        );
    }

    #[tokio::test]
    #[ignore]
    async fn rate_limited_test_has_release_artifact_and_download_artifacts() {
        const RELEASES: [(GhRelease, &[&str]); 1] = [(
            cargo_binstall_v0_20_1::RELEASE,
            cargo_binstall_v0_20_1::ARTIFACTS,
        )];
        const NON_EXISTENT_RELEASES: [GhRelease; 1] = [GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("cargo-bins"),
                repo: CompactString::const_new("cargo-binstall"),
            },
            // We are currently at v0.20.1 and we would never release
            // anything older than v0.20.1
            tag: CompactString::const_new("v0.18.2"),
        }];

        init_logger();

        let mut tasks = Vec::new();

        for client in create_client() {
            async fn has_release_artifact(
                client: &GhApiClient,
                artifact: &GhReleaseArtifact,
            ) -> Result<Option<GhReleaseArtifactUrl>, GhApiError> {
                loop {
                    match client.has_release_artifact(artifact.clone()).await {
                        Err(GhApiError::RateLimit { retry_after }) => {
                            sleep(retry_after.unwrap_or(DEFAULT_RETRY_AFTER)).await
                        }
                        res => break res,
                    }
                }
            }

            for (release, artifacts) in RELEASES {
                for artifact_name in artifacts {
                    let client = client.clone();
                    let release = release.clone();
                    tasks.push(tokio::spawn(async move {
                        let artifact = GhReleaseArtifact {
                            release,
                            artifact_name: artifact_name.to_compact_string(),
                        };

                        let browser_download_task = client.get_auth_token().map(|_| {
                            tokio::spawn(
                                Download::new(
                                    client.remote_client().clone(),
                                    Url::parse(&format!(
                                        "https://github.com/{}/{}/releases/download/{}/{}",
                                        artifact.release.repo.owner,
                                        artifact.release.repo.repo,
                                        artifact.release.tag,
                                        artifact.artifact_name,
                                    ))
                                    .unwrap(),
                                )
                                .into_bytes(),
                            )
                        });
                        let artifact_url = has_release_artifact(&client, &artifact)
                            .await
                            .unwrap()
                            .unwrap();

                        if let Some(browser_download_task) = browser_download_task {
                            let artifact_download_data = loop {
                                match client.download_artifact(artifact_url.clone()).await {
                                    Err(GhApiError::RateLimit { retry_after }) => {
                                        sleep(retry_after.unwrap_or(DEFAULT_RETRY_AFTER)).await
                                    }
                                    res => break res.unwrap(),
                                }
                            }
                            .into_bytes()
                            .await
                            .unwrap();

                            let browser_download_data =
                                browser_download_task.await.unwrap().unwrap();

                            assert_eq!(artifact_download_data, browser_download_data);
                        }
                    }));
                }

                let client = client.clone();
                tasks.push(tokio::spawn(async move {
                    assert_eq!(
                        has_release_artifact(
                            &client,
                            &GhReleaseArtifact {
                                release,
                                artifact_name: "123z".to_compact_string(),
                            }
                        )
                        .await
                        .unwrap(),
                        None
                    );
                }));
            }

            for release in NON_EXISTENT_RELEASES {
                let client = client.clone();

                tasks.push(tokio::spawn(async move {
                    assert_eq!(
                        has_release_artifact(
                            &client,
                            &GhReleaseArtifact {
                                release,
                                artifact_name: "1234".to_compact_string(),
                            }
                        )
                        .await
                        .unwrap(),
                        None
                    );
                }));
            }
        }

        for task in tasks {
            task.await.unwrap();
        }
    }
}
