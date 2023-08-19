use std::sync::{
    atomic::{AtomicBool, Ordering::Relaxed},
    Arc,
};

use derive_destructure2::destructure;

/// Token that can be used to cancel git operation.
#[derive(Clone, Debug, Default)]
pub struct GitCancellationToken(Arc<AtomicBool>);

impl GitCancellationToken {
    /// Create a guard that cancel the git operation on drop.
    #[must_use = "You must assign the guard to a variable, \
otherwise it is equivalent to `GitCancellationToken::cancel()`"]
    pub fn cancel_on_drop(self) -> GitCancelOnDrop {
        GitCancelOnDrop(self)
    }

    /// Cancel the git operation.
    pub fn cancel(&self) {
        self.0.store(true, Relaxed)
    }

    pub(super) fn get_atomic(&self) -> &AtomicBool {
        &self.0
    }
}

/// Guard used to cancel git operation on drop
#[derive(Debug, destructure)]
pub struct GitCancelOnDrop(GitCancellationToken);

impl Drop for GitCancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel()
    }
}
impl GitCancelOnDrop {
    /// Disarm the guard, return the token.
    pub fn disarm(self) -> GitCancellationToken {
        self.destructure().0
    }
}
