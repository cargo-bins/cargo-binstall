use std::{fmt, mem, num::NonZeroU32, path::Path, str::FromStr, sync::atomic::AtomicBool};

use gix::{clone, create, open, remote, Url};
use tracing::debug;

mod progress_tracing;
use progress_tracing::TracingProgress;

mod cancellation_token;
pub use cancellation_token::{GitCancelOnDrop, GitCancellationToken};

mod error;
use error::GitErrorInner;
pub use error::{GitError, GitUrlParseError};

#[derive(Clone, Debug)]
pub struct GitUrl(Url);

impl fmt::Display for GitUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for GitUrl {
    type Err = GitUrlParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Url::try_from(s).map(Self).map_err(GitUrlParseError)
    }
}

#[derive(Debug)]
pub struct Repository(gix::ThreadSafeRepository);

impl Repository {
    fn prepare_fetch(
        url: GitUrl,
        path: &Path,
        kind: create::Kind,
    ) -> Result<clone::PrepareFetch, GitErrorInner> {
        Ok(clone::PrepareFetch::new(
            url.0,
            path,
            kind,
            create::Options {
                destination_must_be_empty: true,
                ..Default::default()
            },
            open::Options::isolated(),
        )?
        .with_shallow(remote::fetch::Shallow::DepthAtRemote(
            NonZeroU32::new(1).unwrap(),
        )))
    }

    /// WARNING: This is a blocking operation, if you want to use it in
    /// async context then you must wrap the call in [`tokio::task::spawn_blocking`].
    ///
    /// WARNING: This function must be called after tokio runtime is initialized.
    pub fn shallow_clone_bare(
        url: GitUrl,
        path: &Path,
        cancellation_token: Option<GitCancellationToken>,
    ) -> Result<Self, GitError> {
        debug!("Shallow cloning {url} to {}", path.display());

        Ok(Self(
            Self::prepare_fetch(url, path, create::Kind::Bare)?
                .fetch_only(
                    &mut TracingProgress::new("Cloning bare"),
                    cancellation_token
                        .as_ref()
                        .map(GitCancellationToken::get_atomic)
                        .unwrap_or(&AtomicBool::new(false)),
                )
                .map_err(GitErrorInner::from)?
                .0
                .into(),
        ))
    }

    /// WARNING: This is a blocking operation, if you want to use it in
    /// async context then you must wrap the call in [`tokio::task::spawn_blocking`].
    ///
    /// WARNING: This function must be called after tokio runtime is initialized.
    pub fn shallow_clone(
        url: GitUrl,
        path: &Path,
        cancellation_token: Option<GitCancellationToken>,
    ) -> Result<Self, GitError> {
        debug!("Shallow cloning {url} to {} with worktree", path.display());

        let mut progress = TracingProgress::new("Cloning with worktree");

        Ok(Self(
            Self::prepare_fetch(url, path, create::Kind::WithWorktree)?
                .fetch_then_checkout(&mut progress, &AtomicBool::new(false))
                .map_err(GitErrorInner::from)?
                .0
                .main_worktree(
                    &mut progress,
                    cancellation_token
                        .as_ref()
                        .map(GitCancellationToken::get_atomic)
                        .unwrap_or(&AtomicBool::new(false)),
                )
                .map_err(GitErrorInner::from)?
                .0
                .into(),
        ))
    }

    #[inline(always)]
    pub fn get_head_commit_entry_data_by_path(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Option<Vec<u8>>, GitError> {
        fn inner(this: &Repository, path: &Path) -> Result<Option<Vec<u8>>, GitErrorInner> {
            Ok(
                if let Some(entry) = this
                    .0
                    .to_thread_local()
                    .head_commit()?
                    .tree()?
                    .peel_to_entry_by_path(path)?
                {
                    Some(mem::take(&mut entry.object()?.data))
                } else {
                    None
                },
            )
        }

        Ok(inner(self, path.as_ref())?)
    }
}
