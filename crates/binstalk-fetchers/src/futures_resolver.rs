use std::{fmt::Debug, future::Future, pin::Pin};

use tokio::sync::mpsc;
use tracing::warn;

/// Given multiple futures with output = `Result<Option<T>, E>`,
/// returns the first `Ok(Some(_))`, logging any `Err(_)`.
pub struct FuturesResolver<T, E> {
    rx: mpsc::Receiver<Result<T, E>>,
    tx: mpsc::Sender<Result<T, E>>,
}

impl<T, E> Default for FuturesResolver<T, E> {
    fn default() -> Self {
        // We only need the first one, so the channel is of size 1.
        let (tx, rx) = mpsc::channel(1);
        Self { tx, rx }
    }
}

impl<T: Send + 'static, E: Send + Debug + 'static> FuturesResolver<T, E> {
    /// Insert new future into this resolver, they will start running
    /// right away.
    pub fn push<Fut>(&self, fut: Fut)
    where
        Fut: Future<Output = Result<Option<T>, E>> + Send + 'static,
    {
        // Boxed, so that the task is the same for every `Fut`, and `fut` is
        // not moved again to be pinned, which would keep room for it twice.
        tokio::spawn(Self::spawn_inner(Box::pin(fut), self.tx.clone()));
    }

    async fn spawn_inner(
        fut: Pin<Box<dyn Future<Output = Result<Option<T>, E>> + Send>>,
        tx: mpsc::Sender<Result<T, E>>,
    ) {
        let res = tokio::select! {
            biased;

            _ = tx.closed() => return,
            res = fut => res,
        };

        if let Some(res) = res.transpose() {
            // Wait for the slot: an error queued first must not displace a
            // success. Sending fails only once the receiver is dropped,
            // i.e. after resolution or cancellation.
            tx.send(res).await.ok();
        }
    }

    /// Insert multiple futures into this resolver, they will start running
    /// right away.
    pub fn extend<Fut, Iter>(&self, iter: Iter)
    where
        Fut: Future<Output = Result<Option<T>, E>> + Send + 'static,
        Iter: IntoIterator<Item = Fut>,
    {
        iter.into_iter().for_each(|fut| self.push(fut));
    }

    /// Return the resolution.
    pub fn resolve(self) -> impl Future<Output = Option<T>> {
        let mut rx = self.rx;
        drop(self.tx);

        async move {
            loop {
                match rx.recv().await {
                    Some(Ok(ret)) => return Some(ret),
                    Some(Err(err)) => warn!(?err, "Fail to resolve the future"),
                    None => return None,
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// A failed candidate's error holding the channel's only slot must not
    /// make a later success get dropped.
    #[tokio::test]
    async fn success_after_queued_error() {
        let resolver = FuturesResolver::<u8, &str>::default();
        resolver.tx.send(Err("failed")).await.unwrap();

        let success = Box::pin(async { Ok(Some(42)) });
        let send = FuturesResolver::spawn_inner(success, resolver.tx.clone());
        // Poll the success before resolve frees the slot.
        let ((), resolved) = tokio::join!(biased; send, resolver.resolve());
        assert_eq!(resolved, Some(42));
    }
}
