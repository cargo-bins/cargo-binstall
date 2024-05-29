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

use binstalk_downloader::remote;
use compact_str::{format_compact, CompactString};
use tokio::sync::OnceCell;

mod common;
mod error;
mod release_artifacts;
mod repo_info;

use common::percent_decode_http_url_path;
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
    pub fn repo_url(&self) -> CompactString {
        format_compact!("https://github.com/{}/{}", self.owner, self.repo)
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

    auth_token: Option<CompactString>,
    is_auth_token_valid: AtomicBool,
}

/// Github API client for querying whether a release artifact exitsts.
/// Can only handle github.com for now.
#[derive(Clone, Debug)]
pub struct GhApiClient(Arc<Inner>);

impl GhApiClient {
    pub fn new(client: remote::Client, auth_token: Option<CompactString>) -> Self {
        Self(Arc::new(Inner {
            client,
            release_artifacts: Default::default(),
            retry_after: Default::default(),

            auth_token,
            is_auth_token_valid: AtomicBool::new(true),
        }))
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
            self.0.auth_token.as_deref()
        } else {
            None
        }
    }

    async fn do_fetch<T, U, GraphQLFn, RestfulFn, GraphQLFut, RestfulFut>(
        &self,
        graphql_func: GraphQLFn,
        restful_func: RestfulFn,
        data: &T,
    ) -> Result<U, GhApiError>
    where
        GraphQLFn: Fn(&remote::Client, &T, &str) -> GraphQLFut,
        RestfulFn: Fn(&remote::Client, &T) -> RestfulFut,
        GraphQLFut: Future<Output = Result<U, GhApiError>> + Send + Sync + 'static,
        RestfulFut: Future<Output = Result<U, GhApiError>> + Send + Sync + 'static,
    {
        self.check_retry_after()?;

        if let Some(auth_token) = self.get_auth_token() {
            match graphql_func(&self.0.client, data, auth_token).await {
                Err(GhApiError::Unauthorized) => {
                    self.0.is_auth_token_valid.store(false, Relaxed);
                }
                res => return res.map_err(|err| err.context("GraphQL API")),
            }
        }

        restful_func(&self.0.client, data)
            .await
            .map_err(|err| err.context("Restful API"))
    }

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

    /// Return `Ok(Some(api_artifact_url))` if exists.
    ///
    /// The returned future is guaranteed to be pointer size.
    pub async fn has_release_artifact(
        &self,
        GhReleaseArtifact {
            release,
            artifact_name,
        }: GhReleaseArtifact,
    ) -> Result<Option<CompactString>, GhApiError> {
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
            Ok(Some(artifacts)) => Ok(artifacts.get_artifact_url(&artifact_name)),
            Ok(None) => Ok(None),
            Err(GhApiError::RateLimit { retry_after }) => {
                *self.0.retry_after.lock().unwrap() =
                    Some(Instant::now() + retry_after.unwrap_or(DEFAULT_RETRY_DURATION));

                Err(GhApiError::RateLimit { retry_after })
            }
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use compact_str::{CompactString, ToCompactString};
    use std::{env, num::NonZeroU16};
    use tracing::subscriber::set_global_default;
    use tracing_subscriber::{filter::LevelFilter, fmt::fmt};

    mod cargo_binstall_v0_20_1 {
        use super::{CompactString, GhRelease, GhRepo};

        pub(super) const RELEASE: GhRelease = GhRelease {
            repo: GhRepo {
                owner: CompactString::new_inline("cargo-bins"),
                repo: CompactString::new_inline("cargo-binstall"),
            },
            tag: CompactString::new_inline("v0.20.1"),
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
                owner: CompactString::new_inline("rustsec"),
                repo: CompactString::new_inline("rustsec"),
            },
            tag: CompactString::new_inline("cargo-audit/v0.17.6"),
        };

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

        #[tokio::test]
        async fn test_gh_api_client_cargo_audit_v_0_17_6() {
            test_specific_release(&RELEASE, ARTIFACTS).await
        }
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
            "https://examle.com",
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
            NonZeroU16::new(10).unwrap(),
            1.try_into().unwrap(),
            [],
        )
        .unwrap()
    }

    /// Mark this as an async fn so that you won't accidentally use it in
    /// sync context.
    async fn create_client() -> Vec<GhApiClient> {
        let client = create_remote_client();

        let mut gh_clients = vec![GhApiClient::new(client.clone(), None)];

        if let Ok(token) = env::var("GITHUB_TOKEN") {
            gh_clients.push(GhApiClient::new(client, Some(token.into())));
        }

        gh_clients
    }

    #[tokio::test]
    async fn test_get_repo_info() {
        const PUBLIC_REPOS: [GhRepo; 1] = [GhRepo {
            owner: CompactString::new_inline("cargo-bins"),
            repo: CompactString::new_inline("cargo-binstall"),
        }];
        const PRIVATE_REPOS: [GhRepo; 1] = [GhRepo {
            owner: CompactString::new_inline("cargo-bins"),
            repo: CompactString::new_inline("private-repo-for-testing"),
        }];
        const NON_EXISTENT_REPOS: [GhRepo; 1] = [GhRepo {
            owner: CompactString::new_inline("cargo-bins"),
            repo: CompactString::new_inline("ttt"),
        }];

        init_logger();

        let mut tests: Vec<(_, _)> = Vec::new();

        for client in create_client().await {
            eprintln!("In client {client:?}");

            for repo in PUBLIC_REPOS {
                let client = client.clone();

                tests.push((
                    Some(RepoInfo::new(repo.clone(), false)),
                    tokio::spawn(async move { client.get_repo_info(&repo).await }),
                ));
            }

            for repo in NON_EXISTENT_REPOS {
                let client = client.clone();

                tests.push((
                    None,
                    tokio::spawn(async move { client.get_repo_info(&repo).await }),
                ));
            }

            if client.get_auth_token().is_some() {
                for repo in PRIVATE_REPOS {
                    let client = client.clone();

                    tests.push((
                        Some(RepoInfo::new(repo.clone(), true)),
                        tokio::spawn(async move { client.get_repo_info(&repo).await }),
                    ));
                }
            }
        }

        for (expected, task) in tests {
            assert_eq!(task.await.unwrap().unwrap(), expected);
        }
    }

    async fn test_specific_release(release: &GhRelease, artifacts: &[&str]) {
        init_logger();

        for client in create_client().await {
            eprintln!("In client {client:?}");

            for artifact_name in artifacts {
                let res = client
                    .has_release_artifact(GhReleaseArtifact {
                        release: release.clone(),
                        artifact_name: artifact_name.to_compact_string(),
                    })
                    .await;

                assert!(
                    matches!(res, Ok(Some(_)) | Err(GhApiError::RateLimit { .. })),
                    "for '{artifact_name}': answer is {:#?}",
                    res
                );
            }

            let res = client
                .has_release_artifact(GhReleaseArtifact {
                    release: release.clone(),
                    artifact_name: "123z".to_compact_string(),
                })
                .await;

            assert!(
                matches!(res, Ok(None) | Err(GhApiError::RateLimit { .. })),
                "res = {:#?}",
                res
            );
        }
    }

    #[tokio::test]
    async fn test_gh_api_client_cargo_binstall_v0_20_1() {
        test_specific_release(
            &cargo_binstall_v0_20_1::RELEASE,
            cargo_binstall_v0_20_1::ARTIFACTS,
        )
        .await
    }

    #[tokio::test]
    async fn test_gh_api_client_cargo_binstall_no_such_release() {
        init_logger();

        for client in create_client().await {
            let release = GhRelease {
                repo: GhRepo {
                    owner: "cargo-bins".to_compact_string(),
                    repo: "cargo-binstall".to_compact_string(),
                },
                // We are currently at v0.20.1 and we would never release
                // anything older than v0.20.1
                tag: "v0.18.2".to_compact_string(),
            };

            let err = client
                .has_release_artifact(GhReleaseArtifact {
                    release,
                    artifact_name: "1234".to_compact_string(),
                })
                .await;

            assert!(
                matches!(
                    err,
                    Ok(None) | Err(GhApiError::NotFound | GhApiError::RateLimit { .. })
                ),
                "err = {:#?}",
                err
            );
        }
    }
}
