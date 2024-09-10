#![allow(unused)]

use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        Once,
    },
};

pub(super) use binstalk_downloader::{
    download::{Download, ExtractedFiles},
    remote::{Client, Url},
};
pub(super) use binstalk_git_repo_api::gh_api_client::GhApiClient;
use binstalk_git_repo_api::gh_api_client::{GhApiError, GhReleaseArtifact, GhReleaseArtifactUrl};
pub(super) use binstalk_types::cargo_toml_binstall::{PkgFmt, PkgMeta};
pub(super) use compact_str::CompactString;
pub(super) use tokio::task::JoinHandle;
pub(super) use tracing::{debug, instrument, warn};

use crate::FetchError;

static WARN_RATE_LIMIT_ONCE: Once = Once::new();
static WARN_UNAUTHORIZED_ONCE: Once = Once::new();

/// Return Ok(Some(api_artifact_url)) if exists, or Ok(None) if it doesn't.
///
/// Caches info on all artifacts matching (repo, tag).
pub(super) async fn get_gh_release_artifact_url(
    gh_api_client: GhApiClient,
    artifact: GhReleaseArtifact,
) -> Result<Option<GhReleaseArtifactUrl>, GhApiError> {
    debug!("Using GitHub API to check for existence of artifact, which will also cache the API response");

    // The future returned has the same size as a pointer
    match gh_api_client.has_release_artifact(artifact).await {
        Ok(ret) => Ok(ret),
        Err(GhApiError::NotFound) => Ok(None),

        Err(GhApiError::RateLimit { retry_after }) => {
            WARN_RATE_LIMIT_ONCE.call_once(|| {
                warn!("Your GitHub API token (if any) has reached its rate limit and cannot be used again until {retry_after:?}, so we will fallback to HEAD/GET on the url.");
                warn!("If you did not supply a github token, consider doing so: GitHub limits unauthorized users to 60 requests per hour per origin IP address.");
            });
            Err(GhApiError::RateLimit { retry_after })
        }
        Err(GhApiError::Unauthorized) => {
            WARN_UNAUTHORIZED_ONCE.call_once(|| {
                warn!("GitHub API somehow requires a token for the API access, so we will fallback to HEAD/GET on the url.");
                warn!("Please consider supplying a token to cargo-binstall to speedup resolution.");
            });
            Err(GhApiError::Unauthorized)
        }

        Err(err) => Err(err),
    }
}

/// Check if the URL exists by querying the GitHub API.
///
/// Caches info on all artifacts matching (repo, tag).
///
/// This function returns a future where its size should be at most size of
/// 2-4 pointers.
pub(super) async fn does_url_exist(
    client: Client,
    gh_api_client: GhApiClient,
    url: &Url,
) -> Result<bool, FetchError> {
    static GH_API_CLIENT_FAILED: AtomicBool = AtomicBool::new(false);

    debug!("Checking for package at: '{url}'");

    if !GH_API_CLIENT_FAILED.load(Relaxed) {
        if let Some(artifact) = GhReleaseArtifact::try_extract_from_url(url) {
            match get_gh_release_artifact_url(gh_api_client, artifact).await {
                Ok(ret) => return Ok(ret.is_some()),

                Err(GhApiError::RateLimit { .. }) | Err(GhApiError::Unauthorized) => {}

                Err(err) => return Err(err.into()),
            }

            GH_API_CLIENT_FAILED.store(true, Relaxed);
        }
    }

    Ok(Box::pin(client.remote_gettable(url.clone())).await?)
}

#[derive(Debug)]
pub(super) struct AutoAbortJoinHandle<T>(JoinHandle<T>);

impl<T> AutoAbortJoinHandle<T>
where
    T: Send + 'static,
{
    pub(super) fn spawn<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        Self(tokio::spawn(future))
    }
}

impl<T> Drop for AutoAbortJoinHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T, E> AutoAbortJoinHandle<Result<T, E>>
where
    E: Into<FetchError>,
{
    pub(super) async fn flattened_join(mut self) -> Result<T, FetchError> {
        (&mut self.0).await?.map_err(Into::into)
    }
}
