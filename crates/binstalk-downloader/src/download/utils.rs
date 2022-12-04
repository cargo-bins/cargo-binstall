use std::future::{pending, Future};

/// Await on `future` if it is not `None`, or call [`pending`]
/// so that this branch would never get selected again.
///
/// Designed to use with [`tokio::select`].
pub(super) async fn await_on_option<Fut, R>(future: Option<Fut>) -> R
where
    Fut: Future<Output = R>,
{
    if let Some(future) = future {
        future.await
    } else {
        pending().await
    }
}
