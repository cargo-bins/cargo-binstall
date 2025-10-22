use std::{
    borrow::Cow,
    fs,
    future::Future,
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

use bytes::Bytes;
use futures_util::Stream;
use tempfile::tempfile as create_tmpfile;
use tokio::sync::mpsc;
use tracing::debug;

use super::{extractor::*, DownloadError, ExtractedFiles, TarBasedFmt};
use crate::{
    download::zip_extraction::do_extract_zip,
    utils::{extract_with_blocking_task, StreamReadable},
};

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

pub async fn extract_tar_based_stream<S>(
    stream: S,
    dst: &Path,
    fmt: TarBasedFmt,
) -> Result<ExtractedFiles, DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin,
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
        // descendants), to ensure that directory permissions do not interfere with descendant
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
