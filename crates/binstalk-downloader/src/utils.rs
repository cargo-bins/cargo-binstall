use std::{future::Future, io};

use bytes::Bytes;
use futures_lite::{
    future::try_zip as try_join,
    stream::{Stream, StreamExt},
};
use tokio::{sync::mpsc, task};

pub(super) fn extract_with_blocking_task<S, F, E>(
    stream: S,
    f: F,
) -> impl Future<Output = Result<(), E>>
where
    E: From<io::Error>,
    S: Stream<Item = Result<Bytes, E>> + Send + Sync + Unpin + 'static,
    F: FnOnce(mpsc::Receiver<Bytes>) -> io::Result<()> + Send + Sync + 'static,
{
    async fn inner<S, Fut, E>(mut stream: S, task: Fut, tx: mpsc::Sender<Bytes>) -> Result<(), E>
    where
        E: From<io::Error>,
        // We do not use trait object for S since there will only be one
        // S used with this function.
        S: Stream<Item = Result<Bytes, E>> + Send + Sync + Unpin + 'static,
        // asyncify would always return the same future, so no need to
        // use trait object here.
        Fut: Future<Output = io::Result<()>> + Send + Sync,
    {
        try_join(
            async move {
                while let Some(bytes) = stream.next().await.transpose()? {
                    if bytes.is_empty() {
                        continue;
                    }

                    if tx.send(bytes).await.is_err() {
                        // The extract tar returns, which could be that:
                        //  - Extraction fails with an error
                        //  - Extraction success without the rest of the data
                        //
                        //
                        // It's hard to tell the difference here, so we assume
                        // the first scienario occurs.
                        //
                        // Even if the second scienario occurs, it won't affect the
                        // extraction process anyway, so we can jsut ignore it.
                        return Ok(());
                    }
                }

                Ok(())
            },
            async move { task.await.map_err(E::from) },
        )
        .await?;

        Ok(())
    }

    // Use channel size = 5 to minimize the waiting time in the extraction task
    let (tx, rx) = mpsc::channel(5);

    let task = asyncify(move || f(rx));

    inner(stream, task, tx)
}

/// Copied from tokio https://docs.rs/tokio/latest/src/tokio/fs/mod.rs.html#132
pub(super) fn asyncify<F, T>(f: F) -> impl Future<Output = io::Result<T>> + Send + Sync + 'static
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    async fn inner<T: Send + 'static>(handle: task::JoinHandle<io::Result<T>>) -> io::Result<T> {
        match handle.await {
            Ok(res) => res,
            Err(err) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("background task failed: {err}"),
            )),
        }
    }

    inner(task::spawn_blocking(f))
}
