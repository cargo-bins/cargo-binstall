use std::{future::Future, sync::Mutex};

use binstalk_git_repo_api::gh_api_client::GhApiClient;
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

use crate::{
    errors::BinstallError,
    helpers::{remote, tasks::AutoAbortJoinHandle},
};

pub type GitHubToken = Option<Zeroizing<Box<str>>>;

#[derive(Debug)]
pub struct LazyGhApiClient {
    client: remote::Client,
    inner: OnceCell<GhApiClient>,
    task: Mutex<Option<AutoAbortJoinHandle<GitHubToken>>>,
}

impl LazyGhApiClient {
    pub fn new(client: remote::Client, auth_token: GitHubToken) -> Self {
        Self {
            inner: OnceCell::new_with(Some(GhApiClient::new(client.clone(), auth_token))),
            client,
            task: Mutex::new(None),
        }
    }

    pub fn with_get_gh_token_future<Fut>(client: remote::Client, get_auth_token_future: Fut) -> Self
    where
        Fut: Future<Output = GitHubToken> + Send + Sync + 'static,
    {
        Self {
            inner: OnceCell::new(),
            task: Mutex::new(Some(AutoAbortJoinHandle::spawn(get_auth_token_future))),
            client,
        }
    }

    pub async fn get(&self) -> Result<&GhApiClient, BinstallError> {
        self.inner
            .get_or_try_init(|| async {
                let task = self.task.lock().unwrap().take();
                Ok(if let Some(task) = task {
                    GhApiClient::new(self.client.clone(), task.await?)
                } else {
                    GhApiClient::new(self.client.clone(), None)
                })
            })
            .await
    }
}
