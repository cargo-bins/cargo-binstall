use std::{
    borrow::Borrow,
    collections::HashSet,
    fmt,
    future::Future,
    hash::{Hash, Hasher},
};

use binstalk_downloader::remote::{self};
use compact_str::{CompactString, ToCompactString};
use serde::Deserialize;
use url::Url;

use super::{
    common::{issue_graphql_query, issue_restful_api},
    GhApiError, GhRelease, GhRepo,
};

// Only include fields we do care about

#[derive(Eq, Deserialize, Debug)]
struct Artifact {
    name: CompactString,
    url: Url,
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
    pub(super) fn get_artifact_url(&self, artifact_name: &str) -> Option<Url> {
        self.assets
            .get(artifact_name)
            .map(|artifact| artifact.url.clone())
    }
}

pub(super) fn fetch_release_artifacts_restful_api(
    client: &remote::Client,
    GhRelease {
        repo: GhRepo { owner, repo },
        tag,
    }: &GhRelease,
    auth_token: Option<&str>,
) -> impl Future<Output = Result<Artifacts, GhApiError>> + Send + 'static {
    issue_restful_api(
        client,
        &["repos", owner, repo, "releases", "tags", tag],
        auth_token,
    )
}

#[derive(Debug, Deserialize)]
struct GraphQLData {
    repository: Option<GraphQLRepo>,
}

#[derive(Debug, Deserialize)]
struct GraphQLRepo {
    release: Option<GraphQLRelease>,
}

#[derive(Debug, Deserialize)]
struct GraphQLRelease {
    #[serde(rename = "releaseAssets")]
    assets: GraphQLReleaseAssets,
}

#[derive(Debug, Deserialize)]
struct GraphQLReleaseAssets {
    nodes: Vec<Artifact>,
    #[serde(rename = "pageInfo")]
    page_info: GraphQLPageInfo,
}

#[derive(Debug, Deserialize)]
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

pub(super) fn fetch_release_artifacts_graphql_api(
    client: &remote::Client,
    GhRelease {
        repo: GhRepo { owner, repo },
        tag,
    }: &GhRelease,
    auth_token: &str,
) -> impl Future<Output = Result<Artifacts, GhApiError>> + Send + 'static {
    let client = client.clone();
    let auth_token = auth_token.to_compact_string();

    let base_query_prefix = format!(
        r#"
query {{
  repository(owner:"{owner}",name:"{repo}") {{
    release(tagName:"{tag}") {{"#
    );

    let base_query_suffix = r#"
  nodes { name url }
  pageInfo { endCursor hasNextPage }
}}}}"#
        .trim();

    async move {
        let mut artifacts = Artifacts::default();
        let mut cond = FilterCondition::Init;
        let base_query_prefix = base_query_prefix.trim();

        loop {
            let query = format!(
                r#"
{base_query_prefix}
releaseAssets({cond}) {{
{base_query_suffix}"#
            );

            let data: GraphQLData = issue_graphql_query(&client, query, &auth_token).await?;

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
                    _ => break Ok(artifacts),
                }
            } else {
                break Err(GhApiError::NotFound);
            }
        }
    }
}
