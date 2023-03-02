use std::{
    borrow::Borrow,
    collections::HashSet,
    hash::{Hash, Hasher},
    io,
    time::Duration,
};

use compact_str::CompactString;
use serde::Deserialize;
use serde_json::from_slice as json_from_slice;
use thiserror::Error as ThisError;
use url::Url;

pub use serde_json::Error as JsonError;

use super::{remote, GhRelease};

#[derive(ThisError, Debug)]
pub enum GhApiError {
    #[error("IO Error: {0}")]
    Io(#[from] io::Error),

    #[error("Failed to parse json: {0}")]
    Json(#[from] JsonError),

    #[error("Remote Error: {0}")]
    Remote(#[from] remote::Error),

    #[error("Failed to parse url: {0}")]
    InvalidUrl(#[from] url::ParseError),
}

// Only include fields we do care about

#[derive(Eq, Deserialize, Debug)]
struct Artifact {
    name: CompactString,
}

// Manually implement PartialEq and Hash to ensure it will always produce the
// same hash as a str with the same content, and that the comparison will be
// the same to coparing a string.

impl PartialEq for Artifact {
    fn eq(&self, other: &Self) -> bool {
        self.name.eq(&other.name)
    }
}

impl Hash for Artifact {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        let s: &str = self.name.as_str();
        s.hash(state)
    }
}

// Implement Borrow so that we can use call
// `HashSet::contains::<str>`

impl Borrow<str> for Artifact {
    fn borrow(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Artifacts {
    assets: HashSet<Artifact>,
}

impl Artifacts {
    pub(super) fn contains(&self, artifact_name: &str) -> bool {
        self.assets.contains(artifact_name)
    }
}

pub(super) enum FetchReleaseRet {
    ReachedRateLimit { retry_after: Option<Duration> },
    ReleaseNotFound,
    Artifacts(Artifacts),
    Unauthorized,
}

/// Returns 404 if not found
pub(super) async fn fetch_release_artifacts(
    client: &remote::Client,
    GhRelease { owner, repo, tag }: GhRelease,
    auth_token: Option<&str>,
) -> Result<FetchReleaseRet, GhApiError> {
    let mut request_builder = client
        .get(Url::parse(&format!(
            "https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}"
        ))?)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");

    if let Some(auth_token) = auth_token {
        request_builder = request_builder.bearer_auth(&auth_token);
    }

    let response = request_builder.send(false).await?;

    let status = response.status();
    let headers = response.headers();

    if status == remote::StatusCode::FORBIDDEN
        && headers
            .get("x-ratelimit-remaining")
            .map(|val| val == "0")
            .unwrap_or(false)
    {
        return Ok(FetchReleaseRet::ReachedRateLimit {
            retry_after: headers.get("x-ratelimit-reset").and_then(|value| {
                let secs = value.to_str().ok()?.parse().ok()?;
                Some(Duration::from_secs(secs))
            }),
        });
    }

    if status == remote::StatusCode::UNAUTHORIZED {
        return Ok(FetchReleaseRet::Unauthorized);
    }

    if status == remote::StatusCode::NOT_FOUND {
        return Ok(FetchReleaseRet::ReleaseNotFound);
    }

    let bytes = response.error_for_status()?.bytes().await?;

    Ok(FetchReleaseRet::Artifacts(json_from_slice(&bytes)?))
}
