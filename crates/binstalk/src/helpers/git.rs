use std::{num::NonZeroU32, path::Path, str::FromStr, sync::atomic::AtomicBool};

use compact_str::CompactString;
use gix::{clone, create, open, remote, Url};
use thiserror::Error as ThisError;
use tracing::debug;

mod progress_tracing;
use progress_tracing::TracingProgress;

pub use gix::url::parse::Error as GitUrlParseError;

#[derive(Debug, ThisError)]
#[non_exhaustive]
pub enum GitError {
    #[error("Failed to prepare for fetch: {0}")]
    PrepareFetchError(#[source] Box<clone::Error>),

    #[error("Failed to fetch: {0}")]
    FetchError(#[source] Box<clone::fetch::Error>),

    #[error("Failed to checkout: {0}")]
    CheckOutError(#[source] Box<clone::checkout::main_worktree::Error>),
}

impl From<clone::Error> for GitError {
    fn from(e: clone::Error) -> Self {
        Self::PrepareFetchError(Box::new(e))
    }
}

impl From<clone::fetch::Error> for GitError {
    fn from(e: clone::fetch::Error) -> Self {
        Self::FetchError(Box::new(e))
    }
}

impl From<clone::checkout::main_worktree::Error> for GitError {
    fn from(e: clone::checkout::main_worktree::Error) -> Self {
        Self::CheckOutError(Box::new(e))
    }
}

#[derive(Clone, Debug)]
pub struct GitUrl(Url);

impl FromStr for GitUrl {
    type Err = GitUrlParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Url::try_from(s).map(Self)
    }
}

#[derive(Debug)]
pub struct Repository(gix::Repository);

impl Repository {
    /// WARNING: This is a blocking operation, if you want to use it in
    /// async context then you must wrap the call in [`tokio::task::spawn_blocking`].
    ///
    /// WARNING: This function must be called after tokio runtime is initialized.
    pub fn shallow_clone(url: GitUrl, path: &Path) -> Result<Self, GitError> {
        let url_bstr = url.0.to_bstring();
        let url_str = String::from_utf8_lossy(&url_bstr);

        debug!("Shallow cloning {url_str} to {}", path.display());

        let mut progress = TracingProgress::new(CompactString::new("Cloning"));

        Ok(Self(
            clone::PrepareFetch::new(
                url.0,
                path,
                create::Kind::WithWorktree,
                create::Options {
                    destination_must_be_empty: true,
                    ..Default::default()
                },
                open::Options::isolated(),
            )?
            .with_shallow(remote::fetch::Shallow::DepthAtRemote(
                NonZeroU32::new(1).unwrap(),
            ))
            .fetch_then_checkout(&mut progress, &AtomicBool::new(false))?
            .0
            .main_worktree(&mut progress, &AtomicBool::new(false))?
            .0,
        ))
    }
}
