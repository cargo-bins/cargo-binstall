use std::{fmt::Debug, future::Future};

use futures_util::stream::{FuturesUnordered, StreamExt};
use tracing::warn;

/// Run `futures` at once and return the output of the first one that returns
/// `Ok(Some(_))`, `None` if none does. Errors are logged and skipped.
///
/// They run in the calling task instead of being spawned, so they may borrow
/// from it. The ones still running when one is found are dropped.
pub(crate) async fn resolve_first<T, E, Fut>(futures: impl IntoIterator<Item = Fut>) -> Option<T>
where
    E: Debug,
    Fut: Future<Output = Result<Option<T>, E>>,
{
    let mut futures: FuturesUnordered<Fut> = futures.into_iter().collect();
    while let Some(res) = futures.next().await {
        match res {
            Ok(Some(ret)) => return Some(ret),
            Ok(None) => (),
            Err(err) => warn!(?err, "Fail to resolve the future"),
        }
    }
    None
}

#[cfg(test)]
mod test {
    use std::{cell::Cell, future::pending};

    use super::*;

    #[tokio::test]
    async fn test_resolve_first() {
        // Borrowing, neither `'static` nor `Send`.
        let checked = Cell::new(0);
        let check = |i: u32| {
            let checked = &checked;
            async move {
                checked.set(checked.get() + 1);
                match i {
                    0 => Err("failed"),
                    1 => Ok(None),
                    _ => Ok(Some(i)),
                }
            }
        };
        assert_eq!(resolve_first([0, 1, 2].map(check)).await, Some(2));
        assert_eq!(checked.get(), 3);

        assert_eq!(resolve_first([0, 1].map(check)).await, None);
        assert_eq!(resolve_first([0u32; 0].map(check)).await, None);

        // Found while another one is still running: that one is dropped.
        let found_or_pending = |found: bool| async move {
            if found {
                Ok::<_, ()>(Some(1))
            } else {
                pending().await
            }
        };
        assert_eq!(
            resolve_first([false, true].map(found_or_pending)).await,
            Some(1)
        );
    }
}
