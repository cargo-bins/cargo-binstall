use std::{fmt, mem, num::NonZeroU32, path::Path, str::FromStr, sync::atomic::AtomicBool};

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

    #[error("HEAD ref was corrupt in crates-io index repository clone")]
    HeadCommit(#[source] Box<gix::reference::head_commit::Error>),

    #[error("tree of head commit wasn't present in crates-io index repository clone")]
    GetTreeOfCommit(#[source] Box<gix::object::commit::Error>),

    #[error("An object was missing in the crates-io index repository clone")]
    ObjectLookup(#[source] Box<gix::object::find::existing::Error>),
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

impl From<gix::reference::head_commit::Error> for GitError {
    fn from(e: gix::reference::head_commit::Error) -> Self {
        Self::HeadCommit(Box::new(e))
    }
}

impl From<gix::object::commit::Error> for GitError {
    fn from(e: gix::object::commit::Error) -> Self {
        Self::GetTreeOfCommit(Box::new(e))
    }
}

impl From<gix::object::find::existing::Error> for GitError {
    fn from(e: gix::object::find::existing::Error) -> Self {
        Self::ObjectLookup(Box::new(e))
    }
}

#[derive(Clone, Debug)]
pub struct GitUrl(Url);

impl fmt::Display for GitUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url_bstr = self.0.to_bstring();
        let url_str = String::from_utf8_lossy(&url_bstr);

        f.write_str(&url_str)
    }
}

impl FromStr for GitUrl {
    type Err = GitUrlParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Url::try_from(s).map(Self)
    }
}

#[derive(Debug)]
pub struct Repository(gix::ThreadSafeRepository);

impl Repository {
    fn prepare_fetch(
        url: GitUrl,
        path: &Path,
        kind: create::Kind,
    ) -> Result<clone::PrepareFetch, GitError> {
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
    pub fn shallow_clone_bare(url: GitUrl, path: &Path) -> Result<Self, GitError> {
        debug!("Shallow cloning {url} to {}", path.display());

        Ok(Self(
            Self::prepare_fetch(url, path, create::Kind::Bare)?
                .fetch_only(
                    &mut TracingProgress::new(CompactString::new("Cloning")),
                    &AtomicBool::new(false),
                )?
                .0
                .into(),
        ))
    }

    /// WARNING: This is a blocking operation, if you want to use it in
    /// async context then you must wrap the call in [`tokio::task::spawn_blocking`].
    ///
    /// WARNING: This function must be called after tokio runtime is initialized.
    pub fn shallow_clone(url: GitUrl, path: &Path) -> Result<Self, GitError> {
        debug!("Shallow cloning {url} to {} with worktree", path.display());

        let mut progress = TracingProgress::new(CompactString::new("Cloning"));

        Ok(Self(
            Self::prepare_fetch(url, path, create::Kind::WithWorktree)?
                .fetch_then_checkout(&mut progress, &AtomicBool::new(false))?
                .0
                .main_worktree(&mut progress, &AtomicBool::new(false))?
                .0
                .into(),
        ))
    }

    #[inline(always)]
    pub fn get_head_commit_entry_data_by_path(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Option<Vec<u8>>, GitError> {
        fn inner(this: &Repository, path: &Path) -> Result<Option<Vec<u8>>, GitError> {
            Ok(
                if let Some(entry) = this
                    .0
                    .to_thread_local()
                    .head_commit()?
                    .tree()?
                    .lookup_entry_by_path(path)?
                {
                    Some(mem::take(&mut entry.object()?.data))
                } else {
                    None
                },
            )
        }

        inner(self, path.as_ref())
    }
}
