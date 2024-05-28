use compact_str::CompactString;
use serde::Deserialize;

use super::{
    common::{issue_graphql_query, issue_restful_api},
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

pub(super) async fn fetch_repo_info_restful_api(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
) -> Result<RepoInfo, GhApiError> {
    issue_restful_api(client, &["repos", owner, repo]).await
}

#[derive(Deserialize)]
struct GraphQLData {
    repository: Option<RepoInfo>,
}

pub(super) async fn fetch_repo_info_graphql_api(
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
