use binstalk_downloader::remote::{header::HeaderMap, StatusCode, Url};
use compact_str::CompactString;
use serde::Deserialize;

use super::{
    common::{self, issue_graphql_query, percent_encode_http_url_path, GraphQLResult},
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

pub(super) type FetchRepoInfoRet = common::GhApiRet<RepoInfo>;

fn check_for_status(status: StatusCode, headers: &HeaderMap) -> Option<FetchRepoInfoRet> {
    common::check_for_status(status, headers)
}

async fn fetch_repo_info_restful_api(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
    auth_token: Option<&str>,
) -> Result<FetchRepoInfoRet, GhApiError> {
    let mut request_builder = client
        .get(Url::parse(&format!(
            "https://api.github.com/repos/{owner}/{repo}",
            owner = percent_encode_http_url_path(owner),
            repo = percent_encode_http_url_path(repo),
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
        Ok(FetchRepoInfoRet::Success(response.json().await?))
    }
}

#[derive(Deserialize)]
struct GraphQLData {
    repository: Option<RepoInfo>,
}

async fn fetch_repo_info_graphql_api(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
    auth_token: &str,
) -> Result<FetchRepoInfoRet, GhApiError> {
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

    match issue_graphql_query(client, query, auth_token).await? {
        GraphQLResult::Data(repo_info) => Ok(common::GhApiRet::Success(repo_info)),
        GraphQLResult::Else(ret) => Ok(ret),
    }
}

pub(super) async fn fetch_repo_info(
    client: &remote::Client,
    repo: &GhRepo,
    auth_token: Option<&str>,
) -> Result<FetchRepoInfoRet, GhApiError> {
    if let Some(auth_token) = auth_token {
        let res = fetch_repo_info_graphql_api(client, repo, auth_token)
            .await
            .map_err(|err| err.context("GraphQL API"));

        match res {
            // Fallback to Restful API
            Ok(FetchRepoInfoRet::Unauthorized) => (),
            res => return res,
        }
    }

    fetch_repo_info_restful_api(client, repo, auth_token)
        .await
        .map_err(|err| err.context("Restful API"))
}
