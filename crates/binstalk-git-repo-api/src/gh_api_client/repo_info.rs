use compact_str::CompactString;
use serde::Deserialize;

use super::{
    common::{issue_graphql_query, issue_restful_api, percent_encode_http_url_path},
    remote, GhApiError, GhRepo,
};

#[derive(Debug, Deserialize)]
struct Owner {
    login: CompactString,
}

#[derive(Debug, Deserialize)]
pub struct RepoInfo {
    owner: Owner,
    name: CompactString,
    private: bool,
}

impl RepoInfo {
    pub fn repo(&self) -> GhRepo {
        GhRepo {
            owner: self.owner.login.clone(),
            repo: self.name.clone(),
        }
    }

    pub fn is_private(&self) -> bool {
        self.private
    }
}

async fn fetch_repo_info_restful_api(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
    auth_token: Option<&str>,
) -> Result<RepoInfo, GhApiError> {
    issue_restful_api(
        client,
        format!(
            "repos/{owner}/{repo}",
            owner = percent_encode_http_url_path(owner),
            repo = percent_encode_http_url_path(repo),
        ),
        auth_token,
    )
    .await
}

#[derive(Deserialize)]
struct GraphQLData {
    repository: Option<RepoInfo>,
}

async fn fetch_repo_info_graphql_api(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
    auth_token: &str,
) -> Result<RepoInfo, GhApiError> {
    let query = format!(
        r#"
query {{
  repository(owner:"{owner}",name:"{repo}") {{
    owner {{
      login
    }}
    name
    private: isPrivate
  }}
}}"#
    );

    issue_graphql_query(client, query, auth_token).await
}

pub(super) async fn fetch_repo_info(
    client: &remote::Client,
    repo: &GhRepo,
    auth_token: Option<&str>,
) -> Result<RepoInfo, GhApiError> {
    if let Some(auth_token) = auth_token {
        let res = fetch_repo_info_graphql_api(client, repo, auth_token)
            .await
            .map_err(|err| err.context("GraphQL API"));

        match res {
            // Fallback to Restful API
            Err(GhApiError::Unauthorized) => (),
            res => return res,
        }
    }

    fetch_repo_info_restful_api(client, repo, auth_token)
        .await
        .map_err(|err| err.context("Restful API"))
}
