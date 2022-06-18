//! # Advantages
//!
//! Using this mod has the following advantages over downloading
//! and extracting in on the async thread:
//!
//!  - The code is pipelined instead of storing the downloaded file in memory
//!    and extract it, except for `PkgFmt::Zip`, since `ZipArchiver::new`
//!    requires `std::io::Seek`, so it fallbacks to writing the a file then
//!    unzip it.
//!  - The async part (downloading) and the extracting part runs in parallel
//!    using `tokio::spawn_nonblocking`.
//!  - Compressing/writing which takes a lot of CPU time will not block
//!    the runtime anymore.
//!  - For all `tar` based formats, it can extract only specified files and
//!    process them in memory, without any disk I/O.

use std::fmt::Debug;
use std::fs;
use std::io::{self, copy, Read, Seek, Write};
use std::path::Path;

use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt};
use log::debug;
use scopeguard::{guard, ScopeGuard};
use tar::Entries;
use tempfile::tempfile;
use tokio::{
    sync::mpsc,
    task::{block_in_place, spawn_blocking, JoinHandle},
};

use super::{extracter::*, readable_rx::*, stream_readable::StreamReadable};
use crate::{BinstallError, TarBasedFmt};

pub(crate) enum Content {
    /// Data to write to file
    Data(Bytes),

    /// Abort the writing and remove the file.
    Abort,
}

/// AsyncExtracter will pass the `Bytes` you give to another thread via
/// a `mpsc` and decompress and unpack it if needed.
///
/// After all write is done, you must call `AsyncExtracter::done`,
/// otherwise the extracted content will be removed on drop.
#[derive(Debug)]
struct AsyncExtracterInner<T> {
    /// Use AutoAbortJoinHandle so that the task
    /// will be cancelled on failure.
    handle: JoinHandle<Result<T, BinstallError>>,
    tx: mpsc::Sender<Content>,
}

impl<T: Debug + Send + 'static> AsyncExtracterInner<T> {
    fn new<F: FnOnce(mpsc::Receiver<Content>) -> Result<T, BinstallError> + Send + 'static>(
        f: F,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Content>(100);

        let handle = spawn_blocking(move || f(rx));

        Self { handle, tx }
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

    async fn done(mut self) -> Result<T, BinstallError> {
        // Drop tx as soon as possible so that the task would wrap up what it
        // was doing and flush out all the pending data.
        drop(self.tx);

        Self::wait(&mut self.handle).await
    }

    async fn wait(handle: &mut JoinHandle<Result<T, BinstallError>>) -> Result<T, BinstallError> {
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

async fn extract_impl<T, S, F, E>(stream: S, f: F) -> Result<T, BinstallError>
where
    T: Debug + Send + 'static,
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    F: FnOnce(StreamReadable<S>) -> Result<T, BinstallError>,
    BinstallError: From<E>,
{
    let readable = StreamReadable::new(stream).await;
    block_in_place(move || f(readable))
}

fn read_into_file(
    file: &mut fs::File,
    rx: &mut mpsc::Receiver<Content>,
) -> Result<(), BinstallError> {
    while let Some(content) = rx.blocking_recv() {
        match content {
            Content::Data(bytes) => file.write_all(&*bytes)?,
            Content::Abort => return Err(io::Error::new(io::ErrorKind::Other, "Aborted").into()),
        }
    }

    file.flush()?;

    Ok(())
}

pub async fn extract_bin<E>(
    stream: impl Stream<Item = Result<Bytes, E>> + Unpin,
    output: &Path,
) -> Result<(), BinstallError>
where
    BinstallError: From<E>,
{
    let path = output.to_owned();

    extract_impl(stream, move |mut reader| {
        fs::create_dir_all(path.parent().unwrap())?;

        let mut file = fs::File::create(&path)?;

        // remove it unless the operation isn't aborted and no write
        // fails.
        let remove_guard = guard(&path, |path| {
            fs::remove_file(path).ok();
        });

        copy(&mut reader, &mut file)?;

        // Operation isn't aborted and all writes succeed,
        // disarm the remove_guard.
        ScopeGuard::into_inner(remove_guard);

        Ok(())
    })
    .await
}

pub async fn extract_zip<E>(
    stream: impl Stream<Item = Result<Bytes, E>> + Unpin,
    output: &Path,
) -> Result<(), BinstallError>
where
    BinstallError: From<E>,
{
    let path = output.to_owned();

    extract_impl(stream, move |mut reader| {
        fs::create_dir_all(path.parent().unwrap())?;

        let mut file = tempfile()?;

        copy(&mut reader, &mut file)?;

        // rewind it so that we can pass it to unzip
        file.rewind()?;

        unzip(file, &path)
    })
    .await
}

pub async fn extract_tar_based_stream<E>(
    stream: impl Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    output: &Path,
    fmt: TarBasedFmt,
) -> Result<(), BinstallError>
where
    BinstallError: From<E>,
{
    let path = output.to_owned();

    extract_impl(stream, move |reader| {
        fs::create_dir_all(path.parent().unwrap())?;

        debug!("Extracting from {fmt} archive to {path:#?}");

        create_tar_decoder(reader, fmt)?.unpack(path)?;

        Ok(())
    })
    .await
}

/// Visitor must iterate over all entries.
/// Entires can be in arbitary order.
pub trait TarEntriesVisitor {
    fn visit<R: Read>(&mut self, entries: Entries<'_, R>) -> Result<(), BinstallError>;
}

impl<V: TarEntriesVisitor> TarEntriesVisitor for &mut V {
    fn visit<R: Read>(&mut self, entries: Entries<'_, R>) -> Result<(), BinstallError> {
        (*self).visit(entries)
    }
}

pub async fn extract_tar_based_stream_and_visit<V: TarEntriesVisitor + Debug + Send + 'static, E>(
    stream: impl Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    fmt: TarBasedFmt,
    mut visitor: V,
) -> Result<V, BinstallError>
where
    BinstallError: From<E>,
{
    extract_impl(stream, move |reader| {
        debug!("Extracting from {fmt} archive to process it in memory");

        let mut tar = create_tar_decoder(reader, fmt)?;
        visitor.visit(tar.entries()?)?;
        Ok(visitor)
    })
    .await
}
