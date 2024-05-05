use std::{
    borrow::Borrow,
    collections::HashSet,
    error, fmt,
    hash::{Hash, Hasher},
    io,
    sync::OnceLock,
    time::Duration,
};

use binstalk_downloader::remote::{header::HeaderMap, StatusCode, Url};
use compact_str::{CompactString, ToCompactString};
use serde::{de::Deserializer, Deserialize, Serialize};
use serde_json::to_string as to_json_string;
use thiserror::Error as ThisError;
use tracing::debug;

use super::{percent_encode_http_url_path, remote, GhRelease};

#[derive(ThisError, Debug)]
#[error("Context: '{context}', err: '{err}'")]
pub struct GhApiContextError {
    context: CompactString,
    #[source]
    err: GhApiError,
}

#[derive(ThisError, Debug)]
#[non_exhaustive]
pub enum GhApiError {
    #[error("IO Error: {0}")]
    Io(#[from] io::Error),

    #[error("Remote Error: {0}")]
    Remote(#[from] remote::Error),

    #[error("Failed to parse url: {0}")]
    InvalidUrl(#[from] url::ParseError),

    /// A wrapped error providing the context the error is about.
    #[error(transparent)]
    Context(Box<GhApiContextError>),

    #[error("Remote failed to process GraphQL query: {0}")]
    GraphQLErrors(#[from] GhGraphQLErrors),
}

impl GhApiError {
    /// Attach context to [`GhApiError`]
    pub fn context(self, context: impl fmt::Display) -> Self {
        Self::Context(Box::new(GhApiContextError {
            context: context.to_compact_string(),
            err: self,
        }))
    }
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

#[derive(Debug, Default, Deserialize)]
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

fn check_for_status(status: StatusCode, headers: &HeaderMap) -> Option<FetchReleaseRet> {
    match status {
        remote::StatusCode::FORBIDDEN
            if headers
                .get("x-ratelimit-remaining")
                .map(|val| val == "0")
                .unwrap_or(false) =>
        {
            Some(FetchReleaseRet::ReachedRateLimit {
                retry_after: headers.get("x-ratelimit-reset").and_then(|value| {
                    let secs = value.to_str().ok()?.parse().ok()?;
                    Some(Duration::from_secs(secs))
                }),
            })
        }

        remote::StatusCode::UNAUTHORIZED => Some(FetchReleaseRet::Unauthorized),
        remote::StatusCode::NOT_FOUND => Some(FetchReleaseRet::ReleaseNotFound),

        _ => None,
    }
}

async fn fetch_release_artifacts_restful_api(
    client: &remote::Client,
    GhRelease { owner, repo, tag }: &GhRelease,
    auth_token: Option<&str>,
) -> Result<FetchReleaseRet, GhApiError> {
    let mut request_builder = client
        .get(Url::parse(&format!(
            "https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}",
            owner = percent_encode_http_url_path(owner),
            repo = percent_encode_http_url_path(repo),
            tag = percent_encode_http_url_path(tag),
        ))?)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");

    if let Some(auth_token) = auth_token {
        request_builder = request_builder.bearer_auth(&auth_token);
    }

    let response = request_builder.send(false).await?;

    if let Some(ret) = check_for_status(response.status(), response.headers()) {
        Ok(ret)
    } else {
        Ok(FetchReleaseRet::Artifacts(response.json().await?))
    }
}

#[derive(Deserialize)]
enum GraphQLResponse {
    #[serde(rename = "data")]
    Data(GraphQLData),

    #[serde(rename = "errors")]
    Errors(GhGraphQLErrors),
}

#[derive(Debug, Deserialize)]
pub struct GhGraphQLErrors(Box<[GraphQLError]>);

impl GhGraphQLErrors {
    fn is_rate_limited(&self) -> bool {
        self.0
            .iter()
            .any(|error| matches!(error.error_type, GraphQLErrorType::RateLimited))
    }
}

impl error::Error for GhGraphQLErrors {}

impl fmt::Display for GhGraphQLErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let last_error_index = self.0.len() - 1;

        for (i, error) in self.0.iter().enumerate() {
            write!(
                f,
                "type: '{error_type}', msg: '{msg}'",
                error_type = error.error_type,
                msg = error.message,
            )?;

            for location in error.locations.as_deref().into_iter().flatten() {
                write!(
                    f,
                    ", occured on query line {line} col {col}",
                    line = location.line,
                    col = location.column
                )?;
            }

            for (k, v) in &error.others {
                write!(f, ", {k}: {v}")?;
            }

            if i < last_error_index {
                f.write_str("\n")?;
            }
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: CompactString,
    locations: Option<Box<[GraphQLLocation]>>,

    #[serde(rename = "type")]
    error_type: GraphQLErrorType,

    #[serde(flatten, with = "tuple_vec_map")]
    others: Vec<(CompactString, serde_json::Value)>,
}

#[derive(Debug)]
enum GraphQLErrorType {
    RateLimited,
    Other(CompactString),
}

impl fmt::Display for GraphQLErrorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GraphQLErrorType::RateLimited => "RATE_LIMITED",
            GraphQLErrorType::Other(s) => s,
        })
    }
}

impl<'de> Deserialize<'de> for GraphQLErrorType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = CompactString::deserialize(deserializer)?;
        Ok(match &*s {
            "RATE_LIMITED" => GraphQLErrorType::RateLimited,
            _ => GraphQLErrorType::Other(s),
        })
    }
}

#[derive(Debug, Deserialize)]
struct GraphQLLocation {
    line: u64,
    column: u64,
}

#[derive(Deserialize)]
struct GraphQLData {
    repository: Option<GraphQLRepo>,
}

#[derive(Deserialize)]
struct GraphQLRepo {
    release: Option<GraphQLRelease>,
}

#[derive(Deserialize)]
struct GraphQLRelease {
    #[serde(rename = "releaseAssets")]
    assets: GraphQLReleaseAssets,
}

#[derive(Deserialize)]
struct GraphQLReleaseAssets {
    nodes: Vec<Artifact>,
    #[serde(rename = "pageInfo")]
    page_info: GraphQLPageInfo,
}

#[derive(Deserialize)]
struct GraphQLPageInfo {
    #[serde(rename = "endCursor")]
    end_cursor: Option<CompactString>,
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
}

enum FilterCondition {
    Init,
    After(CompactString),
}

impl fmt::Display for FilterCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // GitHub imposes a limit of 100 for the value passed to param "first"
            FilterCondition::Init => f.write_str("first:100"),
            FilterCondition::After(end_cursor) => write!(f, r#"first:100,after:"{end_cursor}""#),
        }
    }
}

#[derive(Serialize)]
struct GraphQLQuery {
    query: String,
}

async fn fetch_release_artifacts_graphql_api(
    client: &remote::Client,
    GhRelease { owner, repo, tag }: &GhRelease,
    auth_token: &str,
) -> Result<FetchReleaseRet, GhApiError> {
    static GRAPHQL_ENDPOINT: OnceLock<Url> = OnceLock::new();

    let graphql_endpoint = GRAPHQL_ENDPOINT.get_or_init(|| {
        Url::parse("https://api.github.com/graphql").expect("Literal provided must be a valid url")
    });

    let mut artifacts = Artifacts::default();
    let mut cond = FilterCondition::Init;

    loop {
        let query = format!(
            r#"
query {{
  repository(owner:"{owner}",name:"{repo}") {{
    release(tagName:"{tag}") {{
      releaseAssets({cond}) {{
        nodes {{ name }}
        pageInfo {{ endCursor hasNextPage }}
      }}
    }}
  }}
}}"#
        );

        let graphql_query = to_json_string(&GraphQLQuery { query }).map_err(remote::Error::from)?;

        debug!("Sending graphql query to https://api.github.com/graphql: '{graphql_query}'");

        let request_builder = client
            .post(graphql_endpoint.clone(), graphql_query)
            .header("Accept", "application/vnd.github+json")
            .bearer_auth(&auth_token);

        let response = request_builder.send(false).await?;

        if let Some(ret) = check_for_status(response.status(), response.headers()) {
            return Ok(ret);
        }

        let response: GraphQLResponse = response.json().await?;

        let data = match response {
            GraphQLResponse::Data(data) => data,
            GraphQLResponse::Errors(errors) if errors.is_rate_limited() => {
                return Ok(FetchReleaseRet::ReachedRateLimit { retry_after: None })
            }
            GraphQLResponse::Errors(errors) => return Err(errors.into()),
        };

        let assets = data
            .repository
            .and_then(|repository| repository.release)
            .map(|release| release.assets);

        if let Some(assets) = assets {
            artifacts.assets.extend(assets.nodes);

            match assets.page_info {
                GraphQLPageInfo {
                    end_cursor: Some(end_cursor),
                    has_next_page: true,
                } => {
                    cond = FilterCondition::After(end_cursor);
                }
                _ => break Ok(FetchReleaseRet::Artifacts(artifacts)),
            }
        } else {
            break Ok(FetchReleaseRet::ReleaseNotFound);
        }
    }
}

pub(super) async fn fetch_release_artifacts(
    client: &remote::Client,
    release: &GhRelease,
    auth_token: Option<&str>,
) -> Result<FetchReleaseRet, GhApiError> {
    if let Some(auth_token) = auth_token {
        let res = fetch_release_artifacts_graphql_api(client, release, auth_token)
            .await
            .map_err(|err| err.context("GraphQL API"));

        match res {
            // Fallback to Restful API
            Ok(FetchReleaseRet::Unauthorized) => (),
            res => return res,
        }
    }

    fetch_release_artifacts_restful_api(client, release, auth_token)
        .await
        .map_err(|err| err.context("Restful API"))
}

#[cfg(test)]
mod test {
    use super::*;
    use serde::de::value::{BorrowedStrDeserializer, Error};

    macro_rules! assert_matches {
        ($expression:expr, $pattern:pat $(if $guard:expr)? $(,)?) => {
            match $expression {
                $pattern $(if $guard)? => true,
                expr => {
                    panic!(
                        "assertion failed: `{expr:?}` does not match `{}`",
                        stringify!($pattern $(if $guard)?)
                    )
                }
            }
        }
    }

    #[test]
    fn test_graph_ql_error_type() {
        let deserialize = |input: &str| {
            GraphQLErrorType::deserialize(BorrowedStrDeserializer::<'_, Error>::new(input)).unwrap()
        };

        assert_matches!(deserialize("RATE_LIMITED"), GraphQLErrorType::RateLimited);
        assert_matches!(
            deserialize("rATE_LIMITED"),
            GraphQLErrorType::Other(val) if val == CompactString::new("rATE_LIMITED")
        );
    }
}
