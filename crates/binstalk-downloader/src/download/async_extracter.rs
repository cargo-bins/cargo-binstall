use std::{fs, io::Seek, path::Path};

use bytes::Bytes;
use futures_util::stream::Stream;
use scopeguard::{guard, ScopeGuard};
use tempfile::tempfile;
use tokio::task::block_in_place;
use tracing::debug;

use super::{
    extracter::*, stream_readable::StreamReadable, CancellationFuture, DownloadError, TarBasedFmt,
};

pub async fn extract_bin<S>(
    stream: S,
    path: &Path,
    cancellation_future: CancellationFuture,
) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Unpin + 'static,
{
    let mut reader = StreamReadable::new(stream, cancellation_future).await;
    block_in_place(move || {
        fs::create_dir_all(path.parent().unwrap())?;

        let mut file = fs::File::create(path)?;

        // remove it unless the operation isn't aborted and no write
        // fails.
        let remove_guard = guard(&path, |path| {
            fs::remove_file(path).ok();
        });

        reader.copy(&mut file)?;

        // Operation isn't aborted and all writes succeed,
        // disarm the remove_guard.
        ScopeGuard::into_inner(remove_guard);

        Ok(())
    })
}

pub async fn extract_zip<S>(
    stream: S,
    path: &Path,
    cancellation_future: CancellationFuture,
) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Unpin + 'static,
{
    let mut reader = StreamReadable::new(stream, cancellation_future).await;
    block_in_place(move || {
        fs::create_dir_all(path.parent().unwrap())?;

        let mut file = tempfile()?;

        reader.copy(&mut file)?;

        // rewind it so that we can pass it to unzip
        file.rewind()?;

        unzip(file, path)
    })
}

pub async fn extract_tar_based_stream<S>(
    stream: S,
    path: &Path,
    fmt: TarBasedFmt,
    cancellation_future: CancellationFuture,
) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Unpin + 'static,
{
    let reader = StreamReadable::new(stream, cancellation_future).await;
    block_in_place(move || {
        fs::create_dir_all(path.parent().unwrap())?;

        debug!("Extracting from {fmt} archive to {path:#?}");

        create_tar_decoder(reader, fmt)?.unpack(path)?;

        Ok(())
    })
}
