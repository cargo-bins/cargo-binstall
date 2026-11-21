use std::{
    borrow::Cow,
    fs,
    future::Future,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    pin::Pin,
};

use async_compression::tokio::bufread;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tempfile::tempfile as create_tmpfile;
use tokio::{io::AsyncRead, sync::mpsc};
use tokio_util::io::{ReaderStream, StreamReader};
use tracing::debug;

use super::{CompressionFmt, DownloadError, ExtractedFiles, TarBasedFmt, extractor::*};
use crate::{
    download::zip_extraction::do_extract_zip,
    utils::{StreamReadable, extract_with_blocking_task},
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

/// Decompress a single binary that was compressed without an archive around
/// it, and write it to `path` as [`extract_bin`] does.
pub async fn extract_compressed_bin<S>(
    stream: S,
    path: &Path,
    fmt: CompressionFmt,
) -> Result<ExtractedFiles, DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin,
{
    debug!("Decompressing {fmt} stream");

    let reader = StreamReader::new(stream);

    // A file made by concatenating compressed streams, which `cat a.gz b.gz`
    // gives and parallel compressors such as pigz and pbzip2 write, is still
    // one valid file, so read past the end of the first stream.
    macro_rules! decoder {
        ($decoder:ident) => {{
            let mut decoder = bufread::$decoder::new(reader);
            decoder.multiple_members(true);
            Box::pin(decoder) as Pin<Box<dyn AsyncRead + Send + Sync + '_>>
        }};
    }

    let decoder = match fmt {
        CompressionFmt::Gz => decoder!(GzipDecoder),
        CompressionFmt::Bz2 => decoder!(BzDecoder),
        CompressionFmt::Xz => decoder!(XzDecoder),
        CompressionFmt::Zst => decoder!(ZstdDecoder),
    };

    extract_bin(
        ReaderStream::new(decoder).map(|res| res.map_err(DownloadError::from)),
        path,
    )
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
                // unpack_in returns false if the path contains ".."
                // and is skipped.
                tar::EntryType::Regular if entry.unpack_in(dst)? => {
                    let path = entry.path()?;

                    // create normalized_path in the same way
                    // tar::Entry::unpack_in would normalize the path.
                    let mut normalized_path = PathBuf::new();

                    for part in path.components() {
                        match part {
                            Component::Prefix(..) | Component::RootDir | Component::CurDir => {
                                continue;
                            }

                            // unpack_in would return false if this happens.
                            Component::ParentDir => unreachable!(),

                            Component::Normal(part) => normalized_path.push(part),
                        }
                    }

                    extracted_files.add_file(&normalized_path);
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
) -> impl Future<Output = Result<T, DownloadError>> + use<S, F, T>
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;

    use futures_util::stream;

    fn compress(fmt: CompressionFmt, data: &[u8]) -> Vec<u8> {
        match fmt {
            CompressionFmt::Gz => {
                let mut e =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            CompressionFmt::Bz2 => {
                let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            CompressionFmt::Xz => {
                let mut e = liblzma::write::XzEncoder::new(Vec::new(), 6);
                e.write_all(data).unwrap();
                e.finish().unwrap()
            }
            CompressionFmt::Zst => zstd::stream::encode_all(data, 0).unwrap(),
        }
    }

    /// Feed `data` to the extractor a few bytes at a time, so that the
    /// decoders see stream boundaries in the middle of their headers.
    fn chunked(data: Vec<u8>) -> impl Stream<Item = Result<Bytes, DownloadError>> + Unpin {
        let chunks: Vec<_> = data
            .chunks(7)
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
            .collect();
        stream::iter(chunks)
    }

    const FMTS: [CompressionFmt; 4] = [
        CompressionFmt::Gz,
        CompressionFmt::Bz2,
        CompressionFmt::Xz,
        CompressionFmt::Zst,
    ];

    #[tokio::test]
    async fn compressed_bin_is_decompressed_to_path() {
        let content: Vec<u8> = (0..100_000u32).flat_map(u32::to_le_bytes).collect();

        for fmt in FMTS {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bin-mycrate");

            let extracted = extract_compressed_bin(chunked(compress(fmt, &content)), &path, fmt)
                .await
                .unwrap();

            assert_eq!(fs::read(&path).unwrap(), content, "{fmt}");
            assert!(extracted.has_file(Path::new("bin-mycrate")), "{fmt}");
        }
    }

    #[tokio::test]
    async fn concatenated_streams_are_one_file() {
        for fmt in FMTS {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bin");

            let mut data = compress(fmt, b"hello, ");
            data.extend(compress(fmt, b"world"));

            extract_compressed_bin(chunked(data), &path, fmt)
                .await
                .unwrap();

            assert_eq!(fs::read(&path).unwrap(), b"hello, world", "{fmt}");
        }
    }

    #[tokio::test]
    async fn truncated_stream_is_an_error() {
        for fmt in FMTS {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bin");

            let mut data = compress(fmt, &[0x42; 4096]);
            data.truncate(data.len() / 2);

            extract_compressed_bin(chunked(data), &path, fmt)
                .await
                .unwrap_err();
        }
    }

    #[tokio::test]
    async fn uncompressed_data_is_an_error() {
        for fmt in FMTS {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("bin");

            extract_compressed_bin(chunked(b"\x7fELF not compressed".to_vec()), &path, fmt)
                .await
                .unwrap_err();
        }
    }
}
