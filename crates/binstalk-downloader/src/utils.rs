use std::{future::Future, io};

use bytes::Bytes;
use futures_util::{FutureExt, Stream, StreamExt};
use tokio::{sync::mpsc, task};

/// Run `f`, which extracts from the [`mpsc::Receiver`] passed to it, on a
/// blocking thread while forwarding `stream` to it.
pub(super) fn extract_with_blocking_task<E, StreamError, S, F, T>(
    stream: S,
    f: F,
) -> impl Future<Output = Result<T, E>>
where
    T: Send + 'static,
    E: From<io::Error>,
    E: From<StreamError>,
    S: Stream<Item = Result<Bytes, StreamError>> + Send + Sync + Unpin,
    F: FnOnce(mpsc::Receiver<Bytes>) -> io::Result<T> + Send + Sync + 'static,
{
    extract_with_task(stream, move |rx| asyncify(move || f(rx)))
}

/// Run the future returned by `f`, which extracts from the [`mpsc::Receiver`]
/// passed to it, while forwarding `stream` to it.
pub(super) fn extract_with_task<E, StreamError, S, F, Fut, T>(
    stream: S,
    f: F,
) -> impl Future<Output = Result<T, E>>
where
    E: From<io::Error>,
    E: From<StreamError>,
    S: Stream<Item = Result<Bytes, StreamError>> + Send + Sync + Unpin,
    F: FnOnce(mpsc::Receiver<Bytes>) -> Fut,
    Fut: Future<Output = io::Result<T>> + Send,
{
    // Use channel size = 5 to minimize the waiting time in the extraction task
    let (tx, rx) = mpsc::channel(5);

    forward_stream(stream, f(rx), tx)
}

async fn forward_stream<S, StreamError, Fut, T, E>(
    mut stream: S,
    task: Fut,
    tx: mpsc::Sender<Bytes>,
) -> Result<T, E>
where
    E: From<io::Error>,
    E: From<StreamError>,
    // We do not use trait object for S since there will only be one
    // S used with this function.
    S: Stream<Item = Result<Bytes, StreamError>> + Send + Sync + Unpin,
    Fut: Future<Output = io::Result<T>> + Send,
{
    let read_fut = async move {
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
                // extraction process anyway, so we can just ignore it.
                return Ok(());
            }
        }

        Ok::<_, E>(())
    };
    tokio::pin!(read_fut);

    let task_fut = async move { task.await.map_err(E::from) };
    tokio::pin!(task_fut);

    tokio::select! {
        biased;

        res = &mut read_fut => {
            // The stream reaches eof, propagate error and wait for
            // read task to be done.
            res?;

            task_fut.await
        },
        res = &mut task_fut => {
            // The task finishes before the read task, return early
            // after checking for errors in read_fut.
            if let Some(Err(err)) = read_fut.now_or_never() {
                Err(err)
            } else {
                res
            }
        }
    }
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
            Err(err) => Err(io::Error::other(format!("background task failed: {err}"))),
        }
    }

    inner(task::spawn_blocking(f))
}
