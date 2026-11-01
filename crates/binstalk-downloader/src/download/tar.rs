//! Tar decoding and extraction built on [`tar_codec`].
//!
//! `tar_codec` is a strict tar decoder that rejects ambiguous archives
//! (e.g. mixed GNU/pax framing, conflicting extension state) which other tar
//! implementations may interpret differently, and its extraction is
//! capability-relative (via `cap-std`), so that no member can escape the
//! extraction root through `..`, absolute paths or pre-existing links.

use std::{fmt, fs, io, path::Path, pin::Pin};

use async_compression::tokio::bufread;
use bytes::Bytes;
use futures_util::{stream, Stream};
use tar_codec::{
    extract::{ExtractPolicy, LinkPolicy, SymlinkPolicy},
    Archive, DecodeError, DecodePolicy, ExtractError, Member, MemberMetadata, PaxDecodePolicy,
    PaxVendorExtensionPolicy, TarArchive,
};
use tar_framing::{FrameError, FrameErrorInner};
use thiserror::Error as ThisError;
use tokio::{io::AsyncRead, runtime::Handle};
use tokio_util::io::StreamReader;
use tracing::debug;

mod v7;
use v7::V7ToUstar;

use super::{
    super::utils::extract_with_blocking_task,
    DownloadError, ExtractedFiles,
    TarBasedFmt::{self, *},
};

/// Error returned when decoding or extracting a tar archive fails.
#[derive(ThisError)]
#[error(transparent)]
pub struct TarError(TarErrorInner);

impl fmt::Debug for TarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

#[derive(Debug, ThisError)]
enum TarErrorInner {
    #[error(transparent)]
    Decode(DecodeError),

    #[error(transparent)]
    Extract(ExtractError<DecodeError>),
}

/// Convert a decoding error into [`DownloadError`], unwrapping I/O errors
/// returned by the underlying reader so that errors returned by the
/// download stream (e.g. [`DownloadError::Remote`]) are preserved.
pub(super) fn decode_error(err: DecodeError) -> DownloadError {
    match err {
        DecodeError::Framing(FrameError {
            inner: FrameErrorInner::Io { source },
            ..
        }) => DownloadError::from(source),
        err => DownloadError::Tar(TarError(TarErrorInner::Decode(err))),
    }
}

fn extract_error(err: ExtractError<DecodeError>) -> DownloadError {
    match err {
        ExtractError::Archive(err) => decode_error(err),
        err => DownloadError::Tar(TarError(TarErrorInner::Extract(err))),
    }
}

pub(super) type TarDecoder<'s> = V7ToUstar<Pin<Box<dyn AsyncRead + Send + Sync + 's>>>;

/// Decompress the stream according to `fmt`.
pub(super) fn create_tar_decoder<'s, S>(stream: S, fmt: TarBasedFmt) -> TarDecoder<'s>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + 's,
{
    let reader = StreamReader::new(stream);
    V7ToUstar::new(match fmt {
        Tar => Box::pin(reader),
        Tbz2 => Box::pin(bufread::BzDecoder::new(reader)),
        Tgz => Box::pin(bufread::GzipDecoder::new(reader)),
        Txz => Box::pin(bufread::XzDecoder::new(reader)),
        Tzstd => Box::pin(bufread::ZstdDecoder::new(reader)),
    })
}

/// Create a tar archive decoder with the policy used by binstall.
pub(super) fn create_tar_archive(decoder: TarDecoder<'_>) -> TarArchive<TarDecoder<'_>> {
    TarArchive::new(decoder).with_policy(
        DecodePolicy::default()
            // CPython's tarfile is known to fill uname/gname without the
            // NUL terminator. These fields are never used by binstall.
            .allow_header_uname_and_gname_without_terminator(true)
            .pax_policy(PaxDecodePolicy::default().vendor_extension_policy(
                // bsdtar (default `tar` on macOS) and star record xattrs, ACLs
                // and file flags under these vendor namespaces. They carry no
                // information affecting member framing, path or contents, so
                // they are safe to ignore.
                //
                // Other vendor records (e.g. `GNU.sparse.*`) are still
                // rejected since ignoring them would change the extracted
                // contents.
                PaxVendorExtensionPolicy::ignore(["LIBARCHIVE", "SCHILY"]),
            )),
    )
}

/// Normalize an archive member path the same way `tar::Entry::unpack_in`
/// does: leading `/` and `.` components are stripped.
///
/// Return `None` if the path contains a `..` component.
fn normalize_member_path(path: &str) -> Option<String> {
    let mut normalized = String::with_capacity(path.len());

    for component in path.split('/') {
        match component {
            "" | "." => (),
            ".." => return None,
            component => {
                if !normalized.is_empty() {
                    normalized.push('/');
                }
                normalized.push_str(component);
            }
        }
    }

    Some(normalized)
}

/// An [`Archive`] adapter which only yields regular files and directories,
/// records them in [`ExtractedFiles`] and replaces every other member with a
/// no-op.
///
/// Every member path is normalized before being passed to
/// [`Archive::extract_in`], so that paths recorded are exactly the ones
/// extracted.
struct ExtractedFilesRecorder<'f, A> {
    inner: A,
    extracted_files: &'f mut ExtractedFiles,
}

/// A directory member pointing to the extraction root, which `extract_in`
/// treats as a no-op.
///
/// Used to skip a member without having to loop inside
/// [`Archive::next_member`], which current borrowck does not allow when
/// conditionally returning the borrowed payload.
fn skipped_member<P>(metadata: MemberMetadata) -> Member<P> {
    Member::Directory {
        metadata: MemberMetadata {
            path: String::new(),
            position: metadata.position,
        },
    }
}

impl<A: Archive> Archive for ExtractedFilesRecorder<'_, A> {
    type Error = A::Error;
    type Payload<'a>
        = A::Payload<'a>
    where
        Self: 'a;

    async fn next_member<'a>(
        &'a mut self,
    ) -> Result<Option<Member<Self::Payload<'a>>>, Self::Error> {
        let Some(member) = self.inner.next_member().await? else {
            return Ok(None);
        };

        Ok(Some(match member {
            Member::File {
                metadata,
                size,
                executable,
                payload,
            } => match normalize_member_path(&metadata.path) {
                Some(path) if !path.is_empty() => {
                    self.extracted_files.add_file(Path::new(&path));
                    Member::File {
                        metadata: MemberMetadata {
                            path,
                            position: metadata.position,
                        },
                        size,
                        executable,
                        payload,
                    }
                }
                _ => {
                    debug!("Skipping file with unsafe path {:?}", metadata.path);
                    skipped_member(metadata)
                }
            },
            Member::Directory { metadata } => match normalize_member_path(&metadata.path) {
                Some(path) => {
                    self.extracted_files.add_dir(Path::new(if path.is_empty() {
                        "."
                    } else {
                        &path
                    }));
                    Member::Directory {
                        metadata: MemberMetadata {
                            path,
                            position: metadata.position,
                        },
                    }
                }
                None => {
                    debug!("Skipping directory with unsafe path {:?}", metadata.path);
                    skipped_member(metadata)
                }
            },
            // Links and special files are not supported, skip them.
            Member::SymbolicLink { metadata, .. }
            | Member::HardLink { metadata, .. }
            | Member::Special { metadata, .. } => {
                debug!("Skipping unsupported tar member {:?}", metadata.path);
                skipped_member(metadata)
            }
        }))
    }
}

pub(super) async fn extract_tar_based_stream<S>(
    stream: S,
    dst: &Path,
    fmt: TarBasedFmt,
) -> Result<ExtractedFiles, DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin,
{
    debug!("Extracting from {fmt} archive to {}", dst.display());

    let dst = dst.to_owned();

    // rustc is currently unable to prove that the future returned by
    // `Archive::extract_in` is `Send` (it fails with "implementation of `Send`
    // is not general enough", see rust-lang/rust#110338), so drive it on a
    // blocking thread via `Handle::block_on`, which does not require `Send`.
    //
    // The download stream is still polled on the async runtime and forwarded
    // via a channel.
    extract_with_blocking_task(stream, move |mut rx| {
        Handle::current().block_on(async move {
            fs::create_dir_all(&dst)?;

            let stream = stream::poll_fn(move |cx| rx.poll_recv(cx).map(|bytes| bytes.map(Ok)));

            let mut extracted_files = ExtractedFiles::new();

            ExtractedFilesRecorder {
                inner: create_tar_archive(create_tar_decoder(stream, fmt)),
                extracted_files: &mut extracted_files,
            }
            .extract_in(
                &dst,
                ExtractPolicy::default()
                    .link_policy(LinkPolicy::default().symlink_policy(SymlinkPolicy::Skip)),
            )
            .await
            .map_err(|err| io::Error::from(extract_error(err)))?;

            Ok(extracted_files)
        })
    })
    .await
}

#[cfg(test)]
mod test {
    use super::*;

    use std::{collections::HashSet, ffi::OsStr};

    use tempfile::tempdir;

    use crate::download::ExtractedFilesEntry;

    #[test]
    fn test_normalize_member_path() {
        for (path, expected) in [
            ("a", Some("a")),
            ("./a/b", Some("a/b")),
            ("/a//b/", Some("a/b")),
            ("a/./b/.", Some("a/b")),
            ("./", Some("")),
            ("", Some("")),
            ("a/../b", None),
            ("../a", None),
        ] {
            assert_eq!(normalize_member_path(path).as_deref(), expected, "{path:?}");
        }
    }

    /// Minimal ustar archive writer for tests.
    #[derive(Default)]
    struct TarBuilder(Vec<u8>);

    impl TarBuilder {
        fn header(&mut self, name: &str, typeflag: u8, mode: u32, size: usize, link: &str) {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
            header[108..116].copy_from_slice(b"0000000\0");
            header[116..124].copy_from_slice(b"0000000\0");
            header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
            header[136..148].copy_from_slice(b"00000000000\0");
            header[156] = typeflag;
            header[157..157 + link.len()].copy_from_slice(link.as_bytes());
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");

            header[148..156].copy_from_slice(b"        ");
            let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
            header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());

            self.0.extend_from_slice(&header);
        }

        fn data(&mut self, data: &[u8]) {
            self.0.extend_from_slice(data);
            self.0.resize(self.0.len().next_multiple_of(512), 0);
        }

        fn file(mut self, name: &str, mode: u32, data: &[u8]) -> Self {
            self.header(name, b'0', mode, data.len(), "");
            self.data(data);
            self
        }

        fn entry(mut self, name: &str, typeflag: u8, link: &str) -> Self {
            self.header(name, typeflag, 0o755, 0, link);
            self
        }

        fn pax(mut self, records: &[(&str, &str)]) -> Self {
            let mut data = Vec::new();
            for (key, value) in records {
                // "<len> <key>=<value>\n", where len includes itself.
                let rest = format!(" {key}={value}\n");
                let mut len = rest.len() + 1;
                while format!("{len}").len() + rest.len() != len {
                    len += 1;
                }
                data.extend_from_slice(format!("{len}{rest}").as_bytes());
            }
            self.header("PaxHeaders/entry", b'x', 0o644, data.len(), "");
            self.data(&data);
            self
        }

        fn finish(mut self) -> Bytes {
            self.0.extend_from_slice(&[0; 1024]);
            self.0.into()
        }
    }

    async fn extract(archive: Bytes, dst: &Path) -> Result<ExtractedFiles, DownloadError> {
        extract_tar_based_stream(stream::iter([Ok(archive)]), dst, TarBasedFmt::Tar).await
    }

    fn dir_entry(names: &[&str]) -> ExtractedFilesEntry {
        ExtractedFilesEntry::Dir(Box::new(
            names
                .iter()
                .map(|name| Box::<OsStr>::from(OsStr::new(name)))
                .collect::<HashSet<_>>(),
        ))
    }

    #[tokio::test]
    async fn unsupported_and_unsafe_members_are_skipped() {
        let tempdir = tempdir().unwrap();
        let dst = tempdir.path().join("dst");

        let archive = TarBuilder::default()
            .entry("./", b'5', "")
            .entry("./dir/", b'5', "")
            .file("./dir/bin", 0o755, b"bin")
            .file("/abs", 0o644, b"abs")
            .file("../escape", 0o644, b"escape")
            .file("dir/../../escape2", 0o644, b"escape")
            .entry("symlink", b'2', "../escape")
            .entry("hardlink", b'1', "abs")
            .entry("fifo", b'6', "")
            .finish();

        let extracted_files = extract(archive, &dst).await.unwrap();

        assert_eq!(
            extracted_files.0,
            [
                (Path::new(".").into(), dir_entry(&["dir", "abs"])),
                (Path::new("dir").into(), dir_entry(&["bin"])),
                (Path::new("dir/bin").into(), ExtractedFilesEntry::File),
                (Path::new("abs").into(), ExtractedFilesEntry::File),
            ]
            .into_iter()
            .collect()
        );

        assert_eq!(fs::read(dst.join("dir/bin")).unwrap(), b"bin");
        assert_eq!(fs::read(dst.join("abs")).unwrap(), b"abs");
        assert!(!tempdir.path().join("escape").exists());
        assert!(!tempdir.path().join("escape2").exists());
        for name in ["symlink", "hardlink", "fifo"] {
            assert!(dst.join(name).symlink_metadata().is_err(), "{name}");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = |path| fs::metadata(dst.join(path)).unwrap().permissions().mode();
            assert_ne!(mode("dir/bin") & 0o111, 0);
            assert_eq!(mode("abs") & 0o111, 0);
        }
    }

    #[tokio::test]
    async fn symlink_then_file_does_not_escape() {
        let tempdir = tempdir().unwrap();
        let dst = tempdir.path().join("dst");
        let outside = tempdir.path().join("outside");
        fs::create_dir(&outside).unwrap();

        let archive = TarBuilder::default()
            .entry("link", b'2', outside.to_str().unwrap())
            .file("link/file", 0o644, b"file")
            .finish();

        extract(archive, &dst).await.unwrap();

        assert!(!outside.join("file").exists());
        assert!(dst.join("link").is_dir());
        assert_eq!(fs::read(dst.join("link/file")).unwrap(), b"file");
    }

    #[tokio::test]
    async fn pax_vendor_records() {
        let tempdir = tempdir().unwrap();

        // xattrs recorded by bsdtar on macOS are ignored.
        let archive = TarBuilder::default()
            .pax(&[
                ("LIBARCHIVE.xattr.com.apple.provenance", "AQAA"),
                ("SCHILY.fflags", "nodump"),
                ("path", "a-long-path-from-pax"),
            ])
            .file("short", 0o644, b"data")
            .finish();

        let dst = tempdir.path().join("vendor");
        let extracted_files = extract(archive, &dst).await.unwrap();
        assert!(extracted_files.has_file(Path::new("a-long-path-from-pax")));
        assert_eq!(fs::read(dst.join("a-long-path-from-pax")).unwrap(), b"data");

        // Sparse files are not supported, extracting them as regular files
        // would produce wrong content.
        let archive = TarBuilder::default()
            .pax(&[("GNU.sparse.major", "1"), ("GNU.sparse.minor", "0")])
            .file("sparse", 0o644, b"data")
            .finish();

        let err = extract(archive, &tempdir.path().join("sparse"))
            .await
            .unwrap_err();
        assert!(matches!(err, DownloadError::Tar(_)), "{err:?}");
    }

    #[tokio::test]
    async fn invalid_archive_is_rejected() {
        let tempdir = tempdir().unwrap();

        let mut archive = TarBuilder::default()
            .file("file", 0o644, b"data")
            .finish()
            .to_vec();
        // Corrupt the checksum
        archive[148] = b'7';

        let err = extract(archive.into(), tempdir.path()).await.unwrap_err();
        assert!(matches!(err, DownloadError::Tar(_)), "{err:?}");
    }
}
