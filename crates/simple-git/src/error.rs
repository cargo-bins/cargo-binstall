use gix::clone;
use thiserror::Error as ThisError;

#[derive(Debug, ThisError)]
#[error(transparent)]
pub struct GitError(#[from] GitErrorInner);

#[derive(Debug, ThisError)]
pub(super) enum GitErrorInner {
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

impl From<clone::Error> for GitErrorInner {
    fn from(e: clone::Error) -> Self {
        Self::PrepareFetchError(Box::new(e))
    }
}

impl From<clone::fetch::Error> for GitErrorInner {
    fn from(e: clone::fetch::Error) -> Self {
        Self::FetchError(Box::new(e))
    }
}

impl From<clone::checkout::main_worktree::Error> for GitErrorInner {
    fn from(e: clone::checkout::main_worktree::Error) -> Self {
        Self::CheckOutError(Box::new(e))
    }
}

impl From<gix::reference::head_commit::Error> for GitErrorInner {
    fn from(e: gix::reference::head_commit::Error) -> Self {
        Self::HeadCommit(Box::new(e))
    }
}

impl From<gix::object::commit::Error> for GitErrorInner {
    fn from(e: gix::object::commit::Error) -> Self {
        Self::GetTreeOfCommit(Box::new(e))
    }
}

impl From<gix::object::find::existing::Error> for GitErrorInner {
    fn from(e: gix::object::find::existing::Error) -> Self {
        Self::ObjectLookup(Box::new(e))
    }
}

#[derive(Debug, ThisError)]
#[error(transparent)]
pub struct GitUrlParseError(pub(super) gix::url::parse::Error);
