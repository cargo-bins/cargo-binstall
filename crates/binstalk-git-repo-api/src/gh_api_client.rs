use std::{
    collections::HashMap,
    future::Future,
    ops::Deref,
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant},
};

use binstalk_downloader::{download::Download, remote};
use compact_str::{format_compact, CompactString, ToCompactString};
use tokio::sync::OnceCell;
use tracing::{instrument, Level};
use url::Url;
use zeroize::Zeroizing;

mod common;
mod error;
mod release_artifacts;
mod repo_info;

use common::{check_http_status_and_header, percent_decode_http_url_path};
pub use error::{GhApiContextError, GhApiError, GhGraphQLErrors};
pub use repo_info::RepoInfo;

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
    retry_after: Mutex<Option<Instant>>,

    auth_token: Option<Zeroizing<Box<str>>>,
    is_auth_token_valid: AtomicBool,

    only_use_restful_api: AtomicBool,
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
            retry_after: Default::default(),

            auth_token,
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

        if !self.0.only_use_restful_api.load(Relaxed) {
            if let Some(auth_token) = self.get_auth_token() {
                match graphql_func(&self.0.client, data, auth_token).await {
                    Err(GhApiError::Unauthorized) => {
                        self.0.is_auth_token_valid.store(false, Relaxed);
                    }
                    res => return res.map_err(|err| err.context("GraphQL API")),
                }
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
