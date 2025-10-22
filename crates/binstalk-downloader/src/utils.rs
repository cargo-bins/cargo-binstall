use std::{
    future::Future,
    io::{self, BufRead, Read},
};

use bytes::{Buf, Bytes};
use futures_util::{FutureExt, Stream, StreamExt};
use tokio::{sync::mpsc, task};

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
    async fn inner<S, StreamError, Fut, T, E>(
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
        // asyncify would always return the same future, so no need to
        // use trait object here.
        Fut: Future<Output = io::Result<T>> + Send + Sync,
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
            Err(err) => Err(io::Error::other(format!("background task failed: {err}"))),
        }
    }

    inner(task::spawn_blocking(f))
}

/// This wraps an AsyncIterator as a `Read`able.
/// It must be used in non-async context only,
/// meaning you have to use it with
/// `tokio::task::{block_in_place, spawn_blocking}` or
/// `std::thread::spawn`.
pub(super) struct StreamReadable {
    rx: mpsc::Receiver<Bytes>,
    bytes: Bytes,
}

impl StreamReadable {
    pub(super) fn new(rx: mpsc::Receiver<Bytes>) -> Self {
        Self {
            rx,
            bytes: Bytes::new(),
        }
    }
}

impl Read for StreamReadable {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.fill_buf()?.is_empty() {
            return Ok(0);
        }

        let bytes = &mut self.bytes;

        // copy_to_slice requires the bytes to have enough remaining bytes
        // to fill buf.
        let n = buf.len().min(bytes.remaining());

        // <Bytes as Buf>::copy_to_slice copies and consumes the bytes
        bytes.copy_to_slice(&mut buf[..n]);

        Ok(n)
    }
}

impl BufRead for StreamReadable {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let bytes = &mut self.bytes;

        if !bytes.has_remaining() {
            if let Some(new_bytes) = self.rx.blocking_recv() {
                // new_bytes are guaranteed to be non-empty.
                *bytes = new_bytes;
            }
        }

        Ok(&*bytes)
    }

    fn consume(&mut self, amt: usize) {
        self.bytes.advance(amt);
    }
}
