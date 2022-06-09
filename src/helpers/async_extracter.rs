use std::borrow::Cow;
use std::fs;
use std::io::{self, Seek, Write};
use std::path::Path;

use bytes::Bytes;
use scopeguard::{guard, Always, ScopeGuard};
use tempfile::tempfile;
use tokio::{sync::mpsc, task::spawn_blocking};

use super::{extracter::*, readable_rx::*, AutoAbortJoinHandle};
use crate::{BinstallError, PkgFmt};

pub(crate) enum Content {
    /// Data to write to file
    Data(Bytes),

    /// Abort the writing and remove the file.
    Abort,
}

#[derive(Debug)]
struct AsyncExtracterInner {
    /// Use AutoAbortJoinHandle so that the task
    /// will be cancelled on failure.
    handle: AutoAbortJoinHandle<Result<(), BinstallError>>,
    tx: mpsc::Sender<Content>,
}

impl AsyncExtracterInner {
    ///  * `desired_outputs - If Some(_), then it will filter the tar
    ///    and only extract files specified in it.
    fn new<const N: usize>(
        path: &Path,
        fmt: PkgFmt,
        desired_outputs: Option<[Cow<'static, Path>; N]>,
    ) -> Self {
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
                _ => extract_compressed_from_readable(
                    ReadableRx::new(&mut rx),
                    fmt,
                    &path,
                    desired_outputs.as_ref().map(|arr| &arr[..]),
                )?,
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

    /// Upon error, this extracter shall not be reused.
    /// Otherwise, `Self::done` would panic.
    async fn feed(&mut self, bytes: Bytes) -> Result<(), BinstallError> {
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

/// AsyncExtracter will pass the `Bytes` you give to another thread via
/// a `mpsc` and decompress and unpack it if needed.
///
/// After all write is done, you must call `AsyncExtracter::done`,
/// otherwise the extracted content will be removed on drop.
///
/// # Advantages
///
/// `download_and_extract` has the following advantages over downloading
/// plus extracting in on the same thread:
///
///  - The code is pipelined instead of storing the downloaded file in memory
///    and extract it, except for `PkgFmt::Zip`, since `ZipArchiver::new`
///    requires `std::io::Seek`, so it fallbacks to writing the a file then
///    unzip it.
///  - The async part (downloading) and the extracting part runs in parallel
///    using `tokio::spawn_nonblocking`.
///  - Compressing/writing which takes a lot of CPU time will not block
///    the runtime anymore.
///  - For any PkgFmt except for `PkgFmt::Zip` and `PkgFmt::Bin` (basically
///    all `tar` based formats), it can extract only specified files.
///    This means that `super::drivers::fetch_crate_cratesio` no longer need to
///    extract the whole crate and write them to disk, it now only extract the
///    relevant part (`Cargo.toml`) out to disk and open it.
#[derive(Debug)]
pub struct AsyncExtracter(ScopeGuard<AsyncExtracterInner, fn(AsyncExtracterInner), Always>);

impl AsyncExtracter {
    ///  * `path` - If `fmt` is `PkgFmt::Bin`, then this is the filename
    ///    for the bin.
    ///    Otherwise, it is the directory where the extracted content will be put.
    ///  * `fmt` - The format of the archive to feed in.
    ///  * `desired_outputs - If Some(_), then it will filter the tar and
    ///    only extract files specified in it.
    ///    Note that this is a best-effort and it only works when `fmt`
    ///    is not `PkgFmt::Bin` or `PkgFmt::Zip`.
    pub fn new<const N: usize>(
        path: &Path,
        fmt: PkgFmt,
        desired_outputs: Option<[Cow<'static, Path>; N]>,
    ) -> Self {
        let inner = AsyncExtracterInner::new(path, fmt, desired_outputs);
        Self(guard(inner, AsyncExtracterInner::abort))
    }

    /// Upon error, this extracter shall not be reused.
    /// Otherwise, `Self::done` would panic.
    pub async fn feed(&mut self, bytes: Bytes) -> Result<(), BinstallError> {
        self.0.feed(bytes).await
    }

    pub async fn done(self) -> Result<(), BinstallError> {
        ScopeGuard::into_inner(self.0).done().await
    }
}
