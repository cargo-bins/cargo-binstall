use std::{
    collections::{HashMap, HashSet},
    future::Future,
    ops::{Deref, DerefMut},
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

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct GhRelease {
    pub owner: CompactString,
    pub repo: CompactString,
    pub tag: CompactString,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct GhReleaseArtifact {
    pub release: GhRelease,
    pub artifact_name: CompactString,
}

impl GhReleaseArtifact {
    // https://github.com/cargo-bins/cargo-binstall/releases/download/v0.20.1/cargo-binstall-aarch64-apple-darwin.zip
    pub fn try_extract_from_url(url: &remote::Url) -> Option<Self> {
        if url.domain() != Some("github.com") {
            return None;
        }

        let mut path_segments = url.path_segments()?;

        let owner = path_segments.next()?.to_compact_string();
        let repo = path_segments.next()?.to_compact_string();

        if (path_segments.next()?, path_segments.next()?) != ("releases", "download") {
            return None;
        }

        let tag = path_segments.next()?.to_compact_string();
        let artifact_name = path_segments.next()?.to_compact_string();

        Some(Self {
            release: GhRelease { owner, repo, tag },
            artifact_name,
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
    //  - Support github action token
    //  - Use env var `GITHUB_TOKEN` if present for CI
    //  - Or use a built-in `GITHUB_TOKEN` since the default one is just too limited
    //  - Return an error on reaching rate limit for fallback by checking
    //    `x-ratelimit-remaining` and wait until `x-ratelimit-reset`, in utc sec.
    //
    // Authentication is done using: `Authorization: Bearer my_access_token` or
    // using `Authorization: Bearer my-oauth-token`.

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
    ) -> Result<HasReleaseArtifact, remote::Error> {
        enum Failure {
            Err(GhApiError),
            RateLimit { retry_after: Duration },
        }

        let once_cell = self.0.release_artifacts.get(release.clone());
        let res = once_cell
            .get_or_try_init(|| {
                Box::new(async {
                    use request::FetchReleaseRet::*;

                    {
                        let mut guard = self.0.retry_after.lock().unwrap();

                        if let Some(retry_after) = guard.deref().copied() {
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
                        Ok(ReleaseNotFound) => Ok(None),
                        Ok(Artifacts(artifacts)) => Ok(Some(artifacts)),
                        Ok(ReachedRateLimit { retry_after }) => {
                            Err(Failure::RateLimit { retry_after })
                        }
                        Err(err) => Err(Failure::Err(err)),
                    }
                })
            })
            .await;

        match res {
            Ok(Some(artifacts)) => {
                let has_artifact = artifacts.contains(artifact_name);
                if has_artifact {
                    HasReleaseArtifact::Yes
                } else {
                    HasReleaseArtifact::No
                }
            }
            Ok(None) => Ok(HasReleaseArtifact::NoSuchRelease),
            Err(Failure::RateLimit { retry_after }) => {
                let retry_after = retry_after.unwrap_or(DEFAULT_RETRY_DURATION);

                let now = Instant::now();
                let retry_after = now
                    .checked_add(retry_after)
                    .unwrap_or_else(|| now + DEFAULT_RETRY_DURATION);

                self.0.retry_after().lock().unwrap().deref_mut() = Some(retry_after);

                Ok(HasReleaseArtifact::RateLimit { retry_after })
            }
            Err(Failure::Err(err)) => Err(err),
        }
    }
}

#[derive(Copy, Clone, Debug)]
enum HasReleaseArtifact {
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
