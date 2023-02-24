use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    sync::{Arc, Mutex, RwLock},
    time::{Duration, Instant},
};

use compact_str::{CompactString, ToCompactString};
use tokio::sync::OnceCell;

use crate::remote;

mod request;
pub use request::GhApiError;

/// default retry duration if x-ratelimit-reset is not found in response header
const DEFAULT_RETRY_DURATION: Duration = Duration::from_secs(3);

/// The keys required to identify a github release.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct GhRelease {
    pub owner: CompactString,
    pub repo: CompactString,
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

        (path_segments.next().is_none() && url.fragment().is_none() && url.query().is_none())
            .then_some(Self {
                release: GhRelease {
                    owner: owner.to_compact_string(),
                    repo: repo.to_compact_string(),
                    tag: tag.to_compact_string(),
                },
                artifact_name: artifact_name.to_compact_string(),
            })
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
    auth_token: Option<CompactString>,
    release_artifacts: Map<GhRelease, OnceCell<Option<HashSet<CompactString>>>>,
    retry_after: Mutex<Option<Instant>>,
}

/// Github API client for querying whether a release artifact exitsts.
/// Can only handle github.com for now.
#[derive(Clone, Debug)]
pub struct GhApiClient(Arc<Inner>);

impl GhApiClient {
    // TODO:
    //  - Use env var `GITHUB_TOKEN` if present for CI
    //  - Or use a built-in `GITHUB_TOKEN` since the default one is just too limited

    pub fn new(client: remote::Client, auth_token: Option<CompactString>) -> Self {
        Self(Arc::new(Inner {
            client,
            auth_token,
            release_artifacts: Default::default(),
            retry_after: Default::default(),
        }))
    }

    /// The returned future is guaranteed to be pointer size.
    pub async fn has_release_artifact(
        &self,
        GhReleaseArtifact {
            release,
            artifact_name,
        }: GhReleaseArtifact,
    ) -> Result<HasReleaseArtifact, GhApiError> {
        enum Failure {
            Error(GhApiError),
            RateLimit { retry_after: Instant },
        }

        let once_cell = self.0.release_artifacts.get(release.clone());
        let res = once_cell
            .get_or_try_init(|| {
                Box::pin(async {
                    use request::FetchReleaseRet::*;

                    {
                        let mut guard = self.0.retry_after.lock().unwrap();

                        if let Some(retry_after) = *guard {
                            if retry_after.elapsed().is_zero() {
                                return Err(Failure::RateLimit { retry_after });
                            } else {
                                // Instant retry_after is already reached.
                                *guard = None;
                            }
                        };
                    }

                    match request::fetch_release_artifacts(
                        &self.0.client,
                        release,
                        self.0.auth_token.as_deref(),
                    )
                    .await
                    {
                        Ok(ReleaseNotFound) => Ok::<_, Failure>(None),
                        Ok(Artifacts(artifacts)) => Ok(Some(artifacts)),
                        Ok(ReachedRateLimit { retry_after }) => {
                            let retry_after = retry_after.unwrap_or(DEFAULT_RETRY_DURATION);

                            let now = Instant::now();
                            let retry_after = now
                                .checked_add(retry_after)
                                .unwrap_or_else(|| now + DEFAULT_RETRY_DURATION);

                            Err(Failure::RateLimit { retry_after })
                        }
                        Err(err) => Err(Failure::Error(err)),
                    }
                })
            })
            .await;

        match res {
            Ok(Some(artifacts)) => {
                let has_artifact = artifacts.contains(&artifact_name);
                Ok(if has_artifact {
                    HasReleaseArtifact::Yes
                } else {
                    HasReleaseArtifact::No
                })
            }
            Ok(None) => Ok(HasReleaseArtifact::NoSuchRelease),
            Err(Failure::RateLimit { retry_after }) => {
                *self.0.retry_after.lock().unwrap() = Some(retry_after);

                Ok(HasReleaseArtifact::RateLimit { retry_after })
            }
            Err(Failure::Error(err)) => Err(err),
        }
    }
}

#[derive(Eq, PartialEq, Copy, Clone, Debug)]
pub enum HasReleaseArtifact {
    Yes,
    No,
    NoSuchRelease,

    /// Github rate limit is applied per hour, so in case of reaching the rate
    /// limit, [`GhApiClient`] will return this variant and let the user decide
    /// what to do.
    ///
    /// Usually it is more sensible to fallback to directly HEAD/GET the
    /// artifact url than waiting until `retry_after`.
    ///
    /// If you encounter this frequently, then you should consider getting an
    /// authentication token (can be personal access or oath access token),
    /// which should give you 5000 requests per hour per user.
    ///
    /// Rate limit for unauthorized user is 60 requests per hour per originating
    /// IP address, so it is very easy to be rate limited.
    RateLimit {
        retry_after: Instant,
    },
}

#[cfg(test)]
mod test {
    use super::*;

    mod cargo_binstall_v0_20_1 {
        use super::{CompactString, GhRelease};

        pub(super) const RELEASE: GhRelease = GhRelease {
            owner: CompactString::new_inline("cargo-bins"),
            repo: CompactString::new_inline("cargo-binstall"),
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

        let GhRelease { owner, repo, tag } = RELEASE;

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

        let GhRelease { owner, repo, tag } = RELEASE;

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

    /// Mark this as an async fn so that you won't accidentally use it in
    /// sync context.
    async fn create_client() -> GhApiClient {
        GhApiClient::new(
            remote::Client::new(
                concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
                None,
                Duration::from_millis(10),
                1.try_into().unwrap(),
                [],
            )
            .unwrap(),
            std::env::var("GITHUB_TOKEN").ok().map(CompactString::from),
        )
    }

    #[tokio::test]
    async fn test_gh_api_client_cargo_binstall_v0_20_1() {
        let client = create_client().await;

        let release = cargo_binstall_v0_20_1::RELEASE;

        let artifacts = cargo_binstall_v0_20_1::ARTIFACTS
            .iter()
            .map(ToCompactString::to_compact_string);

        for artifact_name in artifacts {
            let ret = client
                .has_release_artifact(GhReleaseArtifact {
                    release: release.clone(),
                    artifact_name,
                })
                .await
                .unwrap();

            assert!(
                matches!(
                    ret,
                    HasReleaseArtifact::Yes | HasReleaseArtifact::RateLimit { .. }
                ),
                "ret = {:#?}",
                ret
            );
        }

        let ret = client
            .has_release_artifact(GhReleaseArtifact {
                release,
                artifact_name: "123z".to_compact_string(),
            })
            .await
            .unwrap();
        assert_eq!(ret, HasReleaseArtifact::No);
    }

    #[tokio::test]
    async fn test_gh_api_client_cargo_binstall_no_such_release() {
        let client = create_client().await;

        let release = GhRelease {
            owner: "cargo-bins".to_compact_string(),
            repo: "cargo-binstall".to_compact_string(),
            // We are currently at v0.20.1 and we would never release
            // anything older than v0.20.1
            tag: "v0.18.2".to_compact_string(),
        };

        let ret = client
            .has_release_artifact(GhReleaseArtifact {
                release,
                artifact_name: "1234".to_compact_string(),
            })
            .await
            .unwrap();

        assert_eq!(ret, HasReleaseArtifact::NoSuchRelease);
    }
}
