use std::{collections::HashSet, io, time::Duration};

use compact_str::CompactString;
use serde::Deserialize;
use serde_json::Deserializer as JsonDeserializer;
use thiserror::Error as ThisError;
use url::Url;

pub use serde_json::Error as JsonError;

use super::{remote, GhRelease};
use crate::utils::{extract_with_blocking_task, StreamReadable};

#[derive(ThisError, Debug)]
pub enum GhApiError {
    #[error("IO Error: {0}")]
    Io(#[source] io::Error),

    #[error("Failed to parse json: {0}")]
    Json(#[from] JsonError),

    #[error("Remote Error: {0}")]
    Remote(#[from] remote::Error),

    #[error("Failed to parse url: {0}")]
    InvalidUrl(#[from] url::ParseError),
}

impl From<io::Error> for GhApiError {
    fn from(err: io::Error) -> Self {
        match err.get_ref() {
            Some(inner_err) if inner_err.is::<Self>() => {
                let inner = err.into_inner().expect(
                    "err.get_ref() returns Some, so err.into_inner() should also return Some",
                );

                inner.downcast().map(|b| *b).unwrap()
            }
            Some(inner_err) if inner_err.is::<JsonError>() => {
                let inner = err.into_inner().expect(
                    "err.get_ref() returns Some, so err.into_inner() should also return Some",
                );

                inner.downcast().map(|b| GhApiError::Json(*b)).unwrap()
            }
            _ => GhApiError::Io(err),
        }
    }
}

// Only include fields we do care about

#[derive(Deserialize)]
struct Asset {
    name: CompactString,
}

#[derive(Deserialize)]
struct Response {
    assets: Vec<Asset>,
}

pub enum FetchReleaseRet {
    ReachedRateLimit { retry_after: Option<Duration> },
    ReleaseNotFound,
    Artifacts(HashSet<CompactString>),
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

    let stream = response.error_for_status()?.bytes_stream();

    // Restful API will return a lot of data and we don't want to allocate
    // a buffer for all of them.
    //
    // So we instead spawn a blocking task to download and decode json
    // from stream lazily instead of downloading everything into memory
    // then decode it.
    let res: Result<Response, GhApiError> = extract_with_blocking_task(stream, |rx| {
        let reader = StreamReadable::new(rx);
        Response::deserialize(&mut JsonDeserializer::from_reader(reader))
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
    })
    .await;

    let response = res?;

    Ok(FetchReleaseRet::Artifacts(
        response
            .assets
            .into_iter()
            .map(|asset| asset.name)
            .collect(),
    ))
}
