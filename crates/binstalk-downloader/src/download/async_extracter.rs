use std::{
    fs,
    future::Future,
    io::{self, Write},
    path::Path,
};

use async_zip::read::stream::ZipFileReader;
use bytes::{Bytes, BytesMut};
use futures_lite::stream::Stream;
use tokio::sync::mpsc;
use tokio_util::io::StreamReader;
use tracing::debug;

use super::{
    extracter::*, stream_readable::StreamReadable, zip_extraction::extract_zip_entry,
    DownloadError, TarBasedFmt, ZipError,
};
use crate::utils::extract_with_blocking_task;

pub async fn extract_bin<S>(stream: S, path: &Path) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin + 'static,
{
    debug!("Writing to `{}`", path.display());

    extract_with_blocking_decoder(stream, path, |mut rx, path| {
        let mut file = fs::File::create(path)?;

        while let Some(bytes) = rx.blocking_recv() {
            file.write_all(&bytes)?;
        }

        file.flush()
    })
    .await
}

pub async fn extract_zip<S>(stream: S, path: &Path) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Unpin + Send + Sync + 'static,
{
    debug!("Decompressing from zip archive to `{}`", path.display());

    let reader = StreamReader::new(stream);
    let mut zip = ZipFileReader::new(reader);
    let mut buf = BytesMut::with_capacity(4 * 4096);

    while let Some(mut zip_reader) = zip.next_entry().await.map_err(ZipError::from_inner)? {
        extract_zip_entry(&mut zip_reader, path, &mut buf).await?;

        // extract_zip_entry would read the zip_reader until read the file until
        // eof unless extract_zip itself is cancelled or an error is raised.
        //
        // So calling done here should not raise any error.
        zip = zip_reader.done().await.map_err(ZipError::from_inner)?;
    }

    Ok(())
}

pub async fn extract_tar_based_stream<S>(
    stream: S,
    path: &Path,
    fmt: TarBasedFmt,
) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin + 'static,
{
    debug!("Extracting from {fmt} archive to {path:#?}");

    extract_with_blocking_decoder(stream, path, move |rx, path| {
        create_tar_decoder(StreamReadable::new(rx), fmt)?.unpack(path)
    })
    .await
}

fn extract_with_blocking_decoder<S, F>(
    stream: S,
    path: &Path,
    f: F,
) -> impl Future<Output = Result<(), DownloadError>>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin + 'static,
    F: FnOnce(mpsc::Receiver<Bytes>, &Path) -> io::Result<()> + Send + Sync + 'static,
{
    let path = path.to_owned();

    extract_with_blocking_task(stream, move |rx| {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        f(rx, &path)
    })
}
