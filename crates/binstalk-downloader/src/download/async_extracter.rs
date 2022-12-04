use std::{fs, path::Path};

use async_zip::read::stream::ZipFileReader;
use bytes::Bytes;
use futures_util::stream::Stream;
use scopeguard::{guard, ScopeGuard};
use tokio::task::block_in_place;
use tokio_util::io::StreamReader;
use tracing::debug;

use super::{
    extracter::*, stream_readable::StreamReadable, zip_extraction::extract_zip_entry,
    CancellationFuture, DownloadError, TarBasedFmt, ZipError,
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
    S: Stream<Item = Result<Bytes, DownloadError>> + Unpin + Send + Sync + 'static,
{
    debug!("Decompressing from zip archive to `{}`", path.display());

    let extract_future = Box::pin(async move {
        let reader = StreamReader::new(stream);
        let mut zip = ZipFileReader::new(reader);

        while let Some(entry) = zip.entry_reader().await.map_err(ZipError::from_inner)? {
            extract_zip_entry(entry, path).await?;
        }

        Ok(())
    });

    if let Some(cancellation_future) = cancellation_future {
        tokio::select! {
            res = extract_future => res,
            res = cancellation_future => {
                Err(res.err().map(DownloadError::from).unwrap_or(DownloadError::UserAbort))
            }
        }
    } else {
        extract_future.await
    }
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
