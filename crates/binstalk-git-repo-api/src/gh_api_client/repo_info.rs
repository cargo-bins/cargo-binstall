use std::{fmt, future::Future};

use compact_str::CompactString;
use serde::Deserialize;

use super::{
    common::{issue_graphql_query, issue_restful_api},
    remote, GhApiError, GhRepo,
};

#[derive(Clone, Eq, PartialEq, Hash, Debug, Deserialize)]
struct Owner {
    login: CompactString,
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Deserialize)]
pub struct RepoInfo {
    owner: Owner,
    name: CompactString,
    private: bool,
}

impl fmt::Display for RepoInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RepoInfo {{ owner: {}, name: {}, is_private: {} }}",
            self.owner.login, self.name, self.private
        )
    }
}

impl RepoInfo {
    #[cfg(test)]
    pub(crate) fn new(GhRepo { owner, repo }: GhRepo, private: bool) -> Self {
        Self {
            owner: Owner { login: owner },
            name: repo,
            private,
        }
    }
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

pub(super) fn fetch_repo_info_restful_api(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
    auth_token: Option<&str>,
) -> impl Future<Output = Result<Option<RepoInfo>, GhApiError>> + Send + 'static {
    issue_restful_api(client, &["repos", owner, repo], auth_token)
}

#[derive(Debug, Deserialize)]
struct GraphQLData {
    repository: Option<RepoInfo>,
}

pub(super) fn fetch_repo_info_graphql_api(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
    auth_token: &str,
) -> impl Future<Output = Result<Option<RepoInfo>, GhApiError>> + Send + 'static {
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

    let future = issue_graphql_query(client, query, auth_token);

    async move {
        let data: GraphQLData = future.await?;
        Ok(data.repository)
    }
}
