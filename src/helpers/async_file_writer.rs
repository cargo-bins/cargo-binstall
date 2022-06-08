use std::fs;
use std::io::{self, Seek, Write};
use std::path::Path;

use bytes::Bytes;
use scopeguard::{guard, Always, ScopeGuard};
use tempfile::tempfile;
use tokio::{sync::mpsc, task::spawn_blocking};

use super::{extracter::*, readable_rx::*, AutoAbortJoinHandle};
use crate::{BinstallError, PkgFmt};

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
    handle: AutoAbortJoinHandle<Result<(), BinstallError>>,
    tx: mpsc::Sender<Content>,
}

impl AsyncFileWriterInner {
    fn new(path: &Path, fmt: PkgFmt) -> Self {
        let path = path.to_owned();
        let (tx, rx) = mpsc::channel::<Content>(100);

        let handle = AutoAbortJoinHandle::new(spawn_blocking(move || {
            // close rx on error so that tx.send will return an error
            let mut rx = guard(rx, |mut rx| {
                rx.close();
            });

            fs::create_dir_all(path.parent().unwrap())?;

            match fmt {
                PkgFmt::Bin => {
                    let mut file = fs::File::create(&path)?;

                    // remove it unless the operation isn't aborted and no write
                    // fails.
                    let remove_guard = guard(&path, |path| {
                        fs::remove_file(path).ok();
                    });

                    Self::read_into_file(&mut file, &mut rx)?;

                    // Operation isn't aborted and all writes succeed,
                    // disarm the remove_guard.
                    ScopeGuard::into_inner(remove_guard);
                }
                PkgFmt::Zip => {
                    let mut file = tempfile()?;

                    Self::read_into_file(&mut file, &mut rx)?;

                    // rewind it so that we can pass it to unzip
                    file.rewind()?;

                    unzip(file, &path)?;
                }
                _ => extract_compressed_from_readable(ReadableRx::new(&mut rx), fmt, &path)?,
            }

            Ok(())
        }));

        Self { handle, tx }
    }

    fn read_into_file(
        file: &mut fs::File,
        rx: &mut mpsc::Receiver<Content>,
    ) -> Result<(), BinstallError> {
        while let Some(content) = rx.blocking_recv() {
            match content {
                Content::Data(bytes) => file.write_all(&*bytes)?,
                Content::Abort => {
                    return Err(io::Error::new(io::ErrorKind::Other, "Aborted").into())
                }
            }
        }

        file.flush()?;

        Ok(())
    }

    /// Upon error, this writer shall not be reused.
    /// Otherwise, `Self::done` would panic.
    async fn write(&mut self, bytes: Bytes) -> Result<(), BinstallError> {
        if self.tx.send(Content::Data(bytes)).await.is_err() {
            // task failed
            Err(Self::wait(&mut self.handle).await.expect_err(
                "Implementation bug: write task finished successfully before all writes are done",
            ))
        } else {
            Ok(())
        }
    }

    async fn done(mut self) -> Result<(), BinstallError> {
        // Drop tx as soon as possible so that the task would wrap up what it
        // was doing and flush out all the pending data.
        drop(self.tx);

        Self::wait(&mut self.handle).await
    }

    async fn wait(
        handle: &mut AutoAbortJoinHandle<Result<(), BinstallError>>,
    ) -> Result<(), BinstallError> {
        match handle.await {
            Ok(res) => res,
            Err(join_err) => Err(io::Error::new(io::ErrorKind::Other, join_err).into()),
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
    pub fn new(path: &Path, fmt: PkgFmt) -> Self {
        let inner = AsyncFileWriterInner::new(path, fmt);
        Self(guard(inner, AsyncFileWriterInner::abort))
    }

    /// Upon error, this writer shall not be reused.
    /// Otherwise, `Self::done` would panic.
    pub async fn write(&mut self, bytes: Bytes) -> Result<(), BinstallError> {
        self.0.write(bytes).await
    }

    pub async fn done(self) -> Result<(), BinstallError> {
        ScopeGuard::into_inner(self.0).done().await
    }
}
