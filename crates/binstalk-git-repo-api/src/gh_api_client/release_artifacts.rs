use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    hash::{Hash, Hasher},
};

use binstalk_downloader::remote::{self};
use compact_str::{CompactString, ToCompactString};
use serde::Deserialize;
use url::Url;

use super::{
    GhApiError, GhRelease, GhRepo,
    common::{issue_graphql_query, issue_restful_api},
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
) -> impl Future<Output = Result<Artifacts, GhApiError>> + Send + 'static + use<> {
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
) -> impl Future<Output = Result<Artifacts, GhApiError>> + Send + 'static + use<> {
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

#[derive(Debug, Deserialize)]
struct GraphQLBatchData {
    /// Each release under its alias, see [`release_alias`].
    repository: Option<HashMap<CompactString, Option<GraphQLRelease>>>,
}

fn release_alias(index: usize) -> CompactString {
    compact_str::format_compact!("r{index}")
}

fn graphql_string(s: &str) -> String {
    // A JSON string is a valid GraphQL string, with the same escapes.
    serde_json::to_string(s).expect("Serializing a str never fails")
}

fn batch_query(GhRepo { owner, repo }: &GhRepo, tags: &[CompactString]) -> String {
    use std::fmt::Write;

    let mut query = format!(
        "query {{ repository(owner:{},name:{}) {{",
        graphql_string(owner),
        graphql_string(repo)
    );
    for (index, tag) in tags.iter().enumerate() {
        write!(
            query,
            " {}: release(tagName:{}) {{ releaseAssets({}) {{ nodes {{ name url }} pageInfo {{ endCursor hasNextPage }} }} }}",
            release_alias(index),
            graphql_string(tag),
            FilterCondition::Init,
        )
        .expect("Writing to a String never fails");
    }
    query.push_str(" } }");
    query
}

/// What [`fetch_releases_artifacts_graphql_api`] found out about one release.
#[derive(Debug)]
pub(super) enum BatchedRelease {
    Found(Artifacts),
    NotFound,
    /// It has more assets than one page holds, fetch it on its own.
    Incomplete,
}

fn parse_batch(data: GraphQLBatchData, count: usize) -> Result<Vec<BatchedRelease>, GhApiError> {
    let Some(mut releases) = data.repository else {
        return Err(GhApiError::NotFound);
    };

    Ok((0..count)
        .map(
            |index| match releases.remove(&release_alias(index)).flatten() {
                None => BatchedRelease::NotFound,
                Some(GraphQLRelease {
                    assets:
                        GraphQLReleaseAssets {
                            page_info:
                                GraphQLPageInfo {
                                    has_next_page: true,
                                    ..
                                },
                            ..
                        },
                }) => BatchedRelease::Incomplete,
                Some(GraphQLRelease { assets }) => BatchedRelease::Found(Artifacts {
                    assets: assets.nodes.into_iter().collect(),
                }),
            },
        )
        .collect())
}

/// Fetch the assets of every release of `repo` in `tags` with one query.
///
/// Returns one entry per tag, in the same order.
pub(super) fn fetch_releases_artifacts_graphql_api(
    client: &remote::Client,
    repo: &GhRepo,
    tags: &[CompactString],
    auth_token: &str,
) -> impl Future<Output = Result<Vec<BatchedRelease>, GhApiError>> + Send + 'static + use<> {
    let count = tags.len();
    let future = issue_graphql_query(client, batch_query(repo, tags), auth_token);

    async move { parse_batch(future.await?, count) }
}

#[cfg(test)]
mod test {
    use super::*;

    fn repo() -> GhRepo {
        GhRepo {
            owner: CompactString::const_new("rustsec"),
            repo: CompactString::const_new("rustsec"),
        }
    }

    #[test]
    fn test_batch_query() {
        assert_eq!(
            batch_query(
                &repo(),
                &["v0.17.6".into(), r#"cargo-audit/"v0.17.6""#.into()]
            ),
            concat!(
                r#"query { repository(owner:"rustsec",name:"rustsec") {"#,
                r#" r0: release(tagName:"v0.17.6") { releaseAssets(first:100) { nodes { name url } pageInfo { endCursor hasNextPage } } }"#,
                r#" r1: release(tagName:"cargo-audit/\"v0.17.6\"") { releaseAssets(first:100) { nodes { name url } pageInfo { endCursor hasNextPage } } }"#,
                " } }",
            )
        );
    }

    #[test]
    fn test_parse_batch() {
        let data: GraphQLBatchData = serde_json::from_str(
            r#"{"repository": {
                "r0": null,
                "r1": {"releaseAssets": {
                    "nodes": [{"name": "a.tgz", "url": "https://github.com/rustsec/rustsec/releases/download/v1/a.tgz"}],
                    "pageInfo": {"endCursor": "x", "hasNextPage": false}
                }},
                "r2": {"releaseAssets": {
                    "nodes": [],
                    "pageInfo": {"endCursor": "y", "hasNextPage": true}
                }}
            }}"#,
        )
        .unwrap();

        let releases = parse_batch(data, 3).unwrap();
        assert!(matches!(releases[0], BatchedRelease::NotFound));
        assert!(
            matches!(&releases[1], BatchedRelease::Found(artifacts) if artifacts.get_artifact_url("a.tgz").is_some())
        );
        assert!(matches!(releases[2], BatchedRelease::Incomplete));

        let data: GraphQLBatchData = serde_json::from_str(r#"{"repository": null}"#).unwrap();
        assert!(matches!(parse_batch(data, 1), Err(GhApiError::NotFound)));
    }
}
