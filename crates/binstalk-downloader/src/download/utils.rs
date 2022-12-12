use std::{future::Future, io};

use tokio::task;

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
