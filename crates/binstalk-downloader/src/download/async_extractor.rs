use std::{
    fs,
    future::Future,
    io::{self, Write},
    path::Path,
};

use bytes::Bytes;
use futures_util::Stream;
use tempfile::tempfile as create_tmpfile;
use tokio::sync::mpsc;
use tracing::debug;

use super::{DownloadError, ExtractedFiles};
use crate::{download::zip_extraction::do_extract_zip, utils::extract_with_blocking_task};

pub async fn extract_bin<S>(stream: S, path: &Path) -> Result<ExtractedFiles, DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin,
{
    debug!("Writing to `{}`", path.display());

    extract_with_blocking_decoder(stream, path, |rx, path| {
        let mut extracted_files = ExtractedFiles::new();

        extracted_files.add_file(Path::new(path.file_name().unwrap()));

        write_stream_to_file(rx, fs::File::create(path)?)?;

        Ok(extracted_files)
    })
    .await
}

pub async fn extract_zip<S>(stream: S, path: &Path) -> Result<ExtractedFiles, DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Unpin + Send + Sync,
{
    debug!("Downloading from zip archive to tempfile");

    extract_with_blocking_decoder(stream, path, |rx, path| {
        debug!("Decompressing from zip archive to `{}`", path.display());

        do_extract_zip(write_stream_to_file(rx, create_tmpfile()?)?, path).map_err(io::Error::from)
    })
    .await
}

fn extract_with_blocking_decoder<S, F, T>(
    stream: S,
    path: &Path,
    f: F,
) -> impl Future<Output = Result<T, DownloadError>>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin,
    F: FnOnce(mpsc::Receiver<Bytes>, &Path) -> io::Result<T> + Send + Sync + 'static,
    T: Send + 'static,
{
    let path = path.to_owned();

    extract_with_blocking_task(stream, move |rx| {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        f(rx, &path)
    })
}

fn write_stream_to_file(mut rx: mpsc::Receiver<Bytes>, f: fs::File) -> io::Result<fs::File> {
    let mut f = io::BufWriter::new(f);

    while let Some(bytes) = rx.blocking_recv() {
        f.write_all(&bytes)?;
    }

    f.flush()?;

    f.into_inner().map_err(io::IntoInnerError::into_error)
}
