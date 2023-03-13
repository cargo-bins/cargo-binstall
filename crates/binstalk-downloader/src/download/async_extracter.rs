use std::{
    borrow::Cow,
    fs,
    future::Future,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use async_zip::read::stream::ZipFileReader;
use bytes::{Bytes, BytesMut};
use futures_lite::stream::Stream;
use tokio::sync::mpsc;
use tokio_util::io::StreamReader;
use tracing::debug;

use super::{
    extracter::*, zip_extraction::extract_zip_entry, DownloadError, ExtractedFiles, TarBasedFmt,
    ZipError,
};
use crate::utils::{extract_with_blocking_task, StreamReadable};

pub async fn extract_bin<S>(stream: S, path: &Path) -> Result<ExtractedFiles, DownloadError>
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
    .await?;

    let mut extracted_files = ExtractedFiles::new();

    extracted_files.add_file(Path::new(path.file_name().unwrap()));

    Ok(extracted_files)
}

pub async fn extract_zip<S>(stream: S, path: &Path) -> Result<ExtractedFiles, DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Unpin + Send + Sync + 'static,
{
    debug!("Decompressing from zip archive to `{}`", path.display());

    let reader = StreamReader::new(stream);
    let mut zip = ZipFileReader::new(reader);
    let mut buf = BytesMut::with_capacity(4 * 4096);
    let mut extracted_files = ExtractedFiles::new();

    while let Some(mut zip_reader) = zip.next_entry().await.map_err(ZipError::from_inner)? {
        extract_zip_entry(&mut zip_reader, path, &mut buf, &mut extracted_files).await?;

        // extract_zip_entry would read the zip_reader until read the file until
        // eof unless extract_zip itself is cancelled or an error is raised.
        //
        // So calling done here should not raise any error.
        zip = zip_reader.done().await.map_err(ZipError::from_inner)?;
    }

    Ok(extracted_files)
}

pub async fn extract_tar_based_stream<S>(
    stream: S,
    dst: &Path,
    fmt: TarBasedFmt,
) -> Result<ExtractedFiles, DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin + 'static,
{
    debug!("Extracting from {fmt} archive to {}", dst.display());

    extract_with_blocking_decoder(stream, dst, move |rx, dst| {
        // Adapted from https://docs.rs/tar/latest/src/tar/archive.rs.html#189-219

        if dst.symlink_metadata().is_err() {
            fs::create_dir_all(dst)?;
        }

        // Canonicalizing the dst directory will prepend the path with '\\?\'
        // on windows which will allow windows APIs to treat the path as an
        // extended-length path with a 32,767 character limit. Otherwise all
        // unpacked paths over 260 characters will fail on creation with a
        // NotFound exception.
        let dst = &dst
            .canonicalize()
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed(dst));

        let mut tar = create_tar_decoder(StreamReadable::new(rx), fmt)?;
        let mut entries = tar.entries()?;

        let mut extracted_files = ExtractedFiles::new();

        // Delay any directory entries until the end (they will be created if needed by
        // descendants), to ensure that directory permissions do not interfer with descendant
        // extraction.
        let mut directories = Vec::new();

        while let Some(mut entry) = entries.next().transpose()? {
            match entry.header().entry_type() {
                tar::EntryType::Regular => {
                    // unpack_in returns false if the path contains ".."
                    // and is skipped.
                    if entry.unpack_in(dst)? {
                        let path = entry.path()?;

                        // create normalized_path in the same way
                        // tar::Entry::unpack_in would normalize the path.
                        let mut normalized_path = PathBuf::new();

                        for part in path.components() {
                            match part {
                                Component::Prefix(..) | Component::RootDir | Component::CurDir => {
                                    continue
                                }

                                // unpack_in would return false if this happens.
                                Component::ParentDir => unreachable!(),

                                Component::Normal(part) => normalized_path.push(part),
                            }
                        }

                        extracted_files.add_file(&normalized_path);
                    }
                }
                tar::EntryType::Directory => {
                    directories.push(entry);
                }
                _ => (),
            }
        }

        for mut dir in directories {
            if dir.unpack_in(dst)? {
                extracted_files.add_dir(&dir.path()?);
            }
        }

        Ok(extracted_files)
    })
    .await
}

fn extract_with_blocking_decoder<S, F, T>(
    stream: S,
    path: &Path,
    f: F,
) -> impl Future<Output = Result<T, DownloadError>>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin + 'static,
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
