pub use binstalk_downloader::remote::*;
pub use url::ParseError as UrlParseError;

use binstalk_downloader::gh_api_client::{GhApiClient, GhReleaseArtifact, HasReleaseArtifact};
use tracing::{debug, warn};

use crate::errors::BinstallError;

/// This function returns a future where its size should be at most size of
/// 2 pointers.
pub async fn does_url_exist(
    client: Client,
    gh_api_client: GhApiClient,
    url: &Url,
) -> Result<bool, BinstallError> {
    debug!("Checking for package at: '{url}'");

    if let Some(artifact) = GhReleaseArtifact::try_extract_from_url(url) {
        debug!("Using GitHub API to check for existence of artifact, which will also cache the API response");

        // The future returned has the same size as a pointer
        match gh_api_client.has_release_artifact(artifact).await? {
            HasReleaseArtifact::Yes => return Ok(true),
            HasReleaseArtifact::No | HasReleaseArtifact::NoSuchRelease => return Ok(false),

            HasReleaseArtifact::RateLimit { retry_after } => {
                warn!("Your GitHub API token (if any) has reached its rate limit and cannot be used again until {retry_after:?}, so we will fallback to HEAD/GET on the url.");
                warn!("If you did not supply a github token, consider doing so: GitHub limits unauthorized users to 60 requests per hour per origin IP address.");
            }
            HasReleaseArtifact::Unauthorized => {
                warn!("GitHub API somehow requires a token for the API access, so we will fallback to HEAD/GET on the url.");
                warn!("Please consider supplying a token to cargo-binstall to speedup resolution.");
            }
        }
    }

    Ok(Box::pin(client.remote_gettable(url.clone())).await?)
}
