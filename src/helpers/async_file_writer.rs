use std::fs;
use std::io::{self, Write};
use std::path::Path;

use bytes::Bytes;
use scopeguard::{guard, Always, ScopeGuard};
use tokio::{sync::mpsc, task::spawn_blocking};

use super::AutoAbortJoinHandle;

pub enum Content {
    /// Data to write to file
    Data(Bytes),

    /// Abort the writing and remove the file.
    Abort,
}

#[derive(Debug)]
struct AsyncFileWriterInner {
    /// Use AutoAbortJoinHandle so that the task
    /// will be cancelled on failure.
    handle: AutoAbortJoinHandle<io::Result<()>>,
    tx: mpsc::Sender<Content>,
}

impl AsyncFileWriterInner {
    fn new(path: &Path) -> Self {
        let path = path.to_owned();
        let (tx, rx) = mpsc::channel::<Content>(100);

        let handle = AutoAbortJoinHandle::new(spawn_blocking(move || {
            // close rx on error so that tx.send will return an error
            let mut rx = guard(rx, |mut rx| {
                rx.close();
            });

            fs::create_dir_all(path.parent().unwrap())?;
            let mut file = fs::File::create(&path)?;

            // remove it unless the operation isn't aborted and no write
            // fails.
            let remove_guard = guard(path, |path| {
                fs::remove_file(path).ok();
            });

            while let Some(content) = rx.blocking_recv() {
                match content {
                    Content::Data(bytes) => file.write_all(&*bytes)?,
                    Content::Abort => return Err(io::Error::new(io::ErrorKind::Other, "Aborted")),
                }
            }

            file.flush()?;

            // Operation isn't aborted and all writes succeed,
            // disarm the remove_guard.
            ScopeGuard::into_inner(remove_guard);

            Ok(())
        }));

        Self { handle, tx }
    }

    /// Upon error, this writer shall not be reused.
    /// Otherwise, `Self::done` would panic.
    async fn write(&mut self, bytes: Bytes) -> io::Result<()> {
        if self.tx.send(Content::Data(bytes)).await.is_err() {
            // task failed
            Err(Self::wait(&mut self.handle).await.expect_err(
                "Implementation bug: write task finished successfully before all writes are done",
            ))
        } else {
            Ok(())
        }
    }

    async fn done(mut self) -> io::Result<()> {
        // Drop tx as soon as possible so that the task would wrap up what it
        // was doing and flush out all the pending data.
        drop(self.tx);

        Self::wait(&mut self.handle).await
    }

    async fn wait(handle: &mut AutoAbortJoinHandle<io::Result<()>>) -> io::Result<()> {
        match handle.await {
            Ok(res) => res,
            Err(join_err) => Err(io::Error::new(io::ErrorKind::Other, join_err)),
        }
    }

    fn abort(self) {
        let tx = self.tx;
        // If Self::write fail, then the task is already tear down,
        // tx closed and no need to abort.
        if !tx.is_closed() {
            // Use send here because blocking_send would panic if used
            // in async threads.
            tokio::spawn(async move {
                tx.send(Content::Abort).await.ok();
            });
        }
    }
}

/// AsyncFileWriter removes the file if `done` isn't called.
#[derive(Debug)]
pub struct AsyncFileWriter(ScopeGuard<AsyncFileWriterInner, fn(AsyncFileWriterInner), Always>);

impl AsyncFileWriter {
    pub fn new(path: &Path) -> Self {
        let inner = AsyncFileWriterInner::new(path);
        Self(guard(inner, AsyncFileWriterInner::abort))
    }

    /// Upon error, this writer shall not be reused.
    /// Otherwise, `Self::done` would panic.
    pub async fn write(&mut self, bytes: Bytes) -> io::Result<()> {
        self.0.write(bytes).await
    }

    pub async fn done(self) -> io::Result<()> {
        ScopeGuard::into_inner(self.0).done().await
    }
}
