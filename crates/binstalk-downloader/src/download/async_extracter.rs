use std::{
    fs,
    future::Future,
    io::{self, Write},
    path::Path,
};

use async_zip::read::stream::ZipFileReader;
use bytes::{Bytes, BytesMut};
use futures_util::{
    future::try_join,
    stream::{Stream, StreamExt},
};
use tokio::sync::mpsc;
use tokio_util::io::StreamReader;
use tracing::debug;

use super::{
    extracter::*, stream_readable::StreamReadable, utils::asyncify,
    zip_extraction::extract_zip_entry, DownloadError, TarBasedFmt, ZipError,
};

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

async fn extract_with_blocking_decoder<S, F>(
    stream: S,
    path: &Path,
    f: F,
) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin + 'static,
    F: FnOnce(mpsc::Receiver<Bytes>, &Path) -> io::Result<()> + Send + Sync + 'static,
{
    async fn inner<S, Fut>(
        mut stream: S,
        task: Fut,
        tx: mpsc::Sender<Bytes>,
    ) -> Result<(), DownloadError>
    where
        // We do not use trait object for S since there will only be one
        // S used with this function.
        S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin + 'static,
        // asyncify would always return the same future, so no need to
        // use trait object here.
        Fut: Future<Output = io::Result<()>> + Send + Sync,
    {
        try_join(
            async move {
                while let Some(bytes) = stream.next().await.transpose()? {
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
                        // extraction process anyway, so we can jsut ignore it.
                        return Ok(());
                    }
                }

                Ok(())
            },
            task,
        )
        .await?;

        Ok(())
    }

    // Use channel size = 5 to minimize the waiting time in the extraction task
    let (tx, rx) = mpsc::channel(5);

    let path = path.to_owned();

    let task = asyncify(move || {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        f(rx, &path)
    });

    inner(stream, task, tx).await
}
