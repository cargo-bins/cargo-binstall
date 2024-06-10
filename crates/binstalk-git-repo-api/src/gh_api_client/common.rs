use std::{fmt::Debug, future::Future, sync::OnceLock, time::Duration};

use binstalk_downloader::remote::{self, Response, Url};
use compact_str::CompactString;
use percent_encoding::percent_decode_str;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::to_string as to_json_string;
use tracing::debug;

use super::{GhApiError, GhGraphQLErrors};

pub(super) fn percent_decode_http_url_path(input: &str) -> CompactString {
    if input.contains('%') {
        percent_decode_str(input).decode_utf8_lossy().into()
    } else {
        // No '%', no need to decode.
        CompactString::new(input)
    }
}

pub(super) fn check_http_status_and_header(response: &Response) -> Result<(), GhApiError> {
    let headers = response.headers();

    match response.status() {
        remote::StatusCode::FORBIDDEN
            if headers
                .get("x-ratelimit-remaining")
                .map(|val| val == "0")
                .unwrap_or(false) =>
        {
            Err(GhApiError::RateLimit {
                retry_after: headers.get("x-ratelimit-reset").and_then(|value| {
                    let secs = value.to_str().ok()?.parse().ok()?;
                    Some(Duration::from_secs(secs))
                }),
            })
        }

        remote::StatusCode::UNAUTHORIZED => Err(GhApiError::Unauthorized),
        remote::StatusCode::NOT_FOUND => Err(GhApiError::NotFound),

        _ => Ok(()),
    }
}

fn get_api_endpoint() -> &'static Url {
    static API_ENDPOINT: OnceLock<Url> = OnceLock::new();

    API_ENDPOINT.get_or_init(|| {
        Url::parse("https://api.github.com/").expect("Literal provided must be a valid url")
    })
}

pub(super) fn issue_restful_api<T>(
    client: &remote::Client,
    path: &[&str],
) -> impl Future<Output = Result<T, GhApiError>> + Send + Sync + 'static
where
    T: DeserializeOwned,
{
    let mut url = get_api_endpoint().clone();

    url.path_segments_mut()
        .expect("get_api_endpoint() should return a https url")
        .extend(path);

    debug!("Getting restful API: {url}");

    let future = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send(false);

    async move {
        let response = future.await?;

        check_http_status_and_header(&response)?;

        Ok(response.json().await?)
    }
}

#[derive(Debug, Deserialize)]
struct GraphQLResponse<T> {
    data: T,
    errors: Option<GhGraphQLErrors>,
}

#[derive(Serialize)]
struct GraphQLQuery {
    query: String,
}

fn get_graphql_endpoint() -> Url {
    let mut graphql_endpoint = get_api_endpoint().clone();

    graphql_endpoint
        .path_segments_mut()
        .expect("get_api_endpoint() should return a https url")
        .push("graphql");

    graphql_endpoint
}

pub(super) fn issue_graphql_query<T>(
    client: &remote::Client,
    query: String,
    auth_token: &str,
) -> impl Future<Output = Result<T, GhApiError>> + Send + Sync + 'static
where
    T: DeserializeOwned + Debug,
{
    let res = to_json_string(&GraphQLQuery { query })
        .map_err(remote::Error::from)
        .map(|graphql_query| {
            let graphql_endpoint = get_graphql_endpoint();

            debug!("Sending graphql query to {graphql_endpoint}: '{graphql_query}'");

            let request_builder = client
                .post(graphql_endpoint, graphql_query)
                .header("Accept", "application/vnd.github+json")
                .bearer_auth(&auth_token);

            request_builder.send(false)
        });

    async move {
        let response = res?.await?;
        check_http_status_and_header(&response)?;

        let mut response: GraphQLResponse<T> = response.json().await?;

        debug!("response = {response:?}");

        if let Some(error) = response.errors.take() {
            Err(error.into())
        } else {
            Ok(response.data)
        }
    }
}
