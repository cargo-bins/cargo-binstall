use std::{
    borrow::Borrow,
    collections::HashSet,
    fmt,
    hash::{Hash, Hasher},
};

use binstalk_downloader::remote::{self, header::HeaderMap, StatusCode, Url};
use compact_str::CompactString;
use serde::Deserialize;

use super::{
    common::{self, issue_graphql_query, percent_encode_http_url_path, GraphQLResult},
    GhApiError, GhRelease,
};

// Only include fields we do care about

#[derive(Eq, Deserialize, Debug)]
struct Artifact {
    name: CompactString,
    url: CompactString,
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
    /// get url for downloading the artifact using GitHub API (for private repository).
    pub(super) fn get_artifact_url(&self, artifact_name: &str) -> Option<CompactString> {
        self.assets
            .get(artifact_name)
            .map(|artifact| artifact.url.clone())
    }
}

pub(super) type FetchReleaseRet = common::GhApiRet<Artifacts>;

fn check_for_status(status: StatusCode, headers: &HeaderMap) -> Option<FetchReleaseRet> {
    common::check_for_status(status, headers)
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
        Ok(FetchReleaseRet::Success(response.json().await?))
    }
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

async fn fetch_release_artifacts_graphql_api(
    client: &remote::Client,
    GhRelease { owner, repo, tag }: &GhRelease,
    auth_token: &str,
) -> Result<FetchReleaseRet, GhApiError> {
    let mut artifacts = Artifacts::default();
    let mut cond = FilterCondition::Init;

    loop {
        let query = format!(
            r#"
query {{
  repository(owner:"{owner}",name:"{repo}") {{
    release(tagName:"{tag}") {{
      releaseAssets({cond}) {{
        nodes {{
          name
          url
        }}
        pageInfo {{ endCursor hasNextPage }}
      }}
    }}
  }}
}}"#
        );

        let data: GraphQLData = match issue_graphql_query(client, query, auth_token).await? {
            GraphQLResult::Data(data) => data,
            GraphQLResult::Else(ret) => return Ok(ret),
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
                _ => break Ok(FetchReleaseRet::Success(artifacts)),
            }
        } else {
            break Ok(FetchReleaseRet::NotFound);
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
