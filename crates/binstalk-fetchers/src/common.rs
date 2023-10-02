use std::sync::{
    atomic::{AtomicBool, Ordering::Relaxed},
    Once,
};

use binstalk_downloader::gh_api_client::{GhReleaseArtifact, HasReleaseArtifact};
pub(super) use binstalk_downloader::{
    cacher::HTTPCacher,
    download::{Download, ExtractedFiles},
    gh_api_client::GhApiClient,
    remote::{Client, Url},
};
pub(super) use binstalk_types::cargo_toml_binstall::{PkgFmt, PkgMeta};
pub(super) use compact_str::CompactString;
pub(super) use tokio::task::JoinHandle;
pub(super) use tracing::{debug, instrument, warn};

use crate::FetchError;

/// This function returns a future where its size should be at most size of
/// 2-4 pointers.
pub(super) async fn does_url_exist(
    client: Client,
    gh_api_client: GhApiClient,
    url: &Url,
) -> Result<bool, FetchError> {
    static GH_API_CLIENT_FAILED: AtomicBool = AtomicBool::new(false);
    static WARN_RATE_LIMIT_ONCE: Once = Once::new();
    static WARN_UNAUTHORIZED_ONCE: Once = Once::new();

    debug!("Checking for package at: '{url}'");

    if !GH_API_CLIENT_FAILED.load(Relaxed) {
        if let Some(artifact) = GhReleaseArtifact::try_extract_from_url(url) {
            debug!("Using GitHub API to check for existence of artifact, which will also cache the API response");

            // The future returned has the same size as a pointer
            match gh_api_client.has_release_artifact(artifact).await? {
                HasReleaseArtifact::Yes => return Ok(true),
                HasReleaseArtifact::No | HasReleaseArtifact::NoSuchRelease => return Ok(false),

                HasReleaseArtifact::RateLimit { retry_after } => {
                    WARN_RATE_LIMIT_ONCE.call_once(|| {
                        warn!("Your GitHub API token (if any) has reached its rate limit and cannot be used again until {retry_after:?}, so we will fallback to HEAD/GET on the url.");
                        warn!("If you did not supply a github token, consider doing so: GitHub limits unauthorized users to 60 requests per hour per origin IP address.");
                    });
                }
                HasReleaseArtifact::Unauthorized => {
                    WARN_UNAUTHORIZED_ONCE.call_once(|| {
                        warn!("GitHub API somehow requires a token for the API access, so we will fallback to HEAD/GET on the url.");
                        warn!("Please consider supplying a token to cargo-binstall to speedup resolution.");
                    });
                }
            }

            GH_API_CLIENT_FAILED.store(true, Relaxed);
        }
    }

    Ok(Box::pin(client.remote_gettable(url.clone())).await?)
}
