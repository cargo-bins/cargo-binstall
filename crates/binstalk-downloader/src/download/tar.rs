//! Tar decoding and extraction built on [`tar_codec`].
//!
//! `tar_codec` is a strict tar decoder that rejects ambiguous archives
//! (e.g. mixed GNU/pax framing, conflicting extension state) which other tar
//! implementations may interpret differently, and its extraction is
//! capability-relative (via `cap-std`), so that no member can escape the
//! extraction root through `..`, absolute paths or pre-existing links.

use std::{
    fmt,
    future::Future,
    io,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
};

use async_compression::tokio::bufread;
use bytes::Bytes;
use futures_util::{stream, Stream};
use normalize_path::NormalizePath;
use tar_codec::{
    extract::{ExtractPolicy, LinkPolicy, SymlinkPolicy},
    Archive, DecodeError, DecodePolicy, ExtractError, Member, MemberMetadata, MemberPayload,
    PaxDecodePolicy, PaxVendorExtensionPolicy, TarArchive, TarMemberPayload,
};
use tar_framing::{Block, FrameError, FrameErrorInner, BLOCK_SIZE};
use thiserror::Error as ThisError;
use tokio::{
    io::{AsyncRead, AsyncReadExt, Chain},
    sync::mpsc,
};
use tokio_util::io::StreamReader;
use tracing::debug;

mod v7;
use v7::{V7Archive, V7Error, V7Payload};

use super::{
    super::utils::extract_with_task,
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
    Decode(TarDecodeError),

    #[error(transparent)]
    Extract(ExtractError<TarDecodeError>),
}

/// Error returned by [`AnyTarArchive`].
#[derive(Debug, ThisError)]
pub(super) enum TarDecodeError {
    #[error(transparent)]
    Codec(DecodeError),

    #[error(transparent)]
    V7(V7Error),
}

/// Convert a decoding error into [`DownloadError`], unwrapping I/O errors
/// returned by the underlying reader so that errors returned by the
/// download stream (e.g. [`DownloadError::Remote`]) are preserved.
pub(super) fn decode_error(err: TarDecodeError) -> DownloadError {
    match err {
        TarDecodeError::Codec(DecodeError::Framing(FrameError {
            inner: FrameErrorInner::Io { source },
            ..
        }))
        | TarDecodeError::V7(V7Error::Io(source)) => DownloadError::from(source),
        err => DownloadError::Tar(TarError(TarErrorInner::Decode(err))),
    }
}

fn extract_error(err: ExtractError<TarDecodeError>) -> DownloadError {
    match err {
        ExtractError::Archive(err) => decode_error(err),
        err => DownloadError::Tar(TarError(TarErrorInner::Extract(err))),
    }
}

/// Decompressed tar stream.
pub(super) type TarReader<'s> = Pin<Box<dyn AsyncRead + Send + Sync + 's>>;

/// Decompress the stream according to `fmt`.
pub(super) fn create_tar_decoder<'s, S>(stream: S, fmt: TarBasedFmt) -> TarReader<'s>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + 's,
{
    let reader = StreamReader::new(stream);
    match fmt {
        Tar => Box::pin(reader),
        Tbz2 => Box::pin(bufread::BzDecoder::new(reader)),
        Tgz => Box::pin(bufread::GzipDecoder::new(reader)),
        Txz => Box::pin(bufread::XzDecoder::new(reader)),
        Tzstd => Box::pin(bufread::ZstdDecoder::new(reader)),
    }
}

/// The reader of `tar_codec`: the first block, already read to detect the
/// format, followed by the rest of the stream.
type CodecReader<'s> = Chain<io::Cursor<Vec<u8>>, TarReader<'s>>;

/// A tar archive, decoded by `tar_codec`, or by [`V7Archive`] for legacy v7
/// archives which `tar_codec` deliberately does not support.
pub(super) enum AnyTarArchive<'s> {
    Codec(Box<TarArchive<CodecReader<'s>>>),
    V7(V7Archive<TarReader<'s>>),
}

pub(super) enum AnyTarPayload<'a, 's> {
    Codec(TarMemberPayload<'a, CodecReader<'s>>),
    V7(V7Payload<'a, TarReader<'s>>),
}

fn map_payload<P, Q>(member: Member<P>, f: impl FnOnce(P) -> Q) -> Member<Q> {
    match member {
        Member::File {
            metadata,
            size,
            executable,
            payload,
        } => Member::File {
            metadata,
            size,
            executable,
            payload: f(payload),
        },
        Member::HardLink {
            metadata,
            target,
            size,
            payload,
        } => Member::HardLink {
            metadata,
            target,
            size,
            payload: f(payload),
        },
        Member::Directory { metadata } => Member::Directory { metadata },
        Member::SymbolicLink { metadata, target } => Member::SymbolicLink { metadata, target },
        Member::Special { metadata, kind } => Member::Special { metadata, kind },
    }
}

impl<'s> Archive for AnyTarArchive<'s> {
    type Error = TarDecodeError;
    type Payload<'a>
        = AnyTarPayload<'a, 's>
    where
        Self: 'a;

    async fn next_member<'a>(
        &'a mut self,
    ) -> Result<Option<Member<Self::Payload<'a>>>, Self::Error> {
        Ok(match self {
            Self::Codec(archive) => archive
                .next_member()
                .await
                .map_err(TarDecodeError::Codec)?
                .map(|member| map_payload(member, AnyTarPayload::Codec)),
            Self::V7(archive) => archive
                .next_member()
                .await
                .map_err(TarDecodeError::V7)?
                .map(|member| map_payload(member, AnyTarPayload::V7)),
        })
    }
}

impl MemberPayload for AnyTarPayload<'_, '_> {
    type Error = TarDecodeError;

    async fn next_chunk(
        &mut self,
        buffer: &mut Vec<u8>,
        target_len: usize,
    ) -> Result<bool, Self::Error> {
        match self {
            Self::Codec(payload) => payload
                .next_chunk(buffer, target_len)
                .await
                .map_err(TarDecodeError::Codec),
            Self::V7(payload) => payload
                .next_chunk(buffer, target_len)
                .await
                .map_err(TarDecodeError::V7),
        }
    }

    async fn skip(self) -> Result<(), Self::Error> {
        match self {
            Self::Codec(payload) => payload.skip().await.map_err(TarDecodeError::Codec),
            Self::V7(payload) => payload.skip().await.map_err(TarDecodeError::V7),
        }
    }
}

/// Detect the format from the first block and create the corresponding
/// decoder with the policy used by binstall.
pub(super) async fn open_tar_archive(
    mut reader: TarReader<'_>,
) -> Result<AnyTarArchive<'_>, DownloadError> {
    let mut first: Block = [0; BLOCK_SIZE];
    let mut filled = 0;
    while filled < BLOCK_SIZE {
        let n = reader.read(&mut first[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }

    if filled == BLOCK_SIZE && v7::is_v7_header(&first) {
        debug!("Detected legacy v7 tar archive");
        return Ok(AnyTarArchive::V7(V7Archive::new(reader, first)));
    }

    let reader = io::Cursor::new(first[..filled].to_vec()).chain(reader);

    Ok(AnyTarArchive::Codec(Box::new(
        TarArchive::new(reader).with_policy(
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
        ),
    )))
}

/// Normalize an archive member path using [`NormalizePath::try_normalize`],
/// the same way as zip extraction.
///
/// Return `None` if the path is absolute or escapes the extraction root, which
/// itself is returned as an empty path.
fn normalize_member_path(path: &str) -> Option<String> {
    let mut normalized = String::with_capacity(path.len());

    for component in Path::new(path).try_normalize()?.components() {
        // `try_normalize` only returns normal components, or `.` for the root.
        if let Component::Normal(component) = component {
            if !normalized.is_empty() {
                // `extract_in` uses `/` as separator on all platforms.
                normalized.push('/');
            }
            normalized.push_str(component.to_str()?);
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
///
/// `extracted_files` is shared instead of borrowed so that the recorder has no
/// lifetime, see [`extract_tar`].
struct ExtractedFilesRecorder<A> {
    inner: A,
    extracted_files: Arc<Mutex<ExtractedFiles>>,
}

fn record(extracted_files: &Mutex<ExtractedFiles>, f: impl FnOnce(&mut ExtractedFiles)) {
    f(&mut extracted_files
        .lock()
        .unwrap_or_else(PoisonError::into_inner))
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

impl<A: Archive> Archive for ExtractedFilesRecorder<A> {
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
                    record(&self.extracted_files, |files| {
                        files.add_file(Path::new(&path))
                    });
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
                    record(&self.extracted_files, |files| {
                        files.add_dir(Path::new(if path.is_empty() { "." } else { &path }))
                    });
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

    extract_with_task(stream, |rx| extract_tar(rx, dst.to_owned(), fmt)).await
}

/// [`AnyTarArchive`] without lifetime, see [`extract_tar`].
struct StaticTarArchive(AnyTarArchive<'static>);

impl Archive for StaticTarArchive {
    type Error = TarDecodeError;
    type Payload<'a> = AnyTarPayload<'a, 'static>;

    async fn next_member<'a>(
        &'a mut self,
    ) -> Result<Option<Member<Self::Payload<'a>>>, Self::Error> {
        self.0.next_member().await
    }
}

/// Extract the tar archive received from `rx` to `dst`.
///
/// rustc is currently unable to prove that the future returned by
/// [`Archive::extract_in`] is `Send` when the archive type has a lifetime (it
/// fails with "implementation of `Send` is not general enough", see
/// rust-lang/rust#110338). So the archive is decoded from a channel rather
/// than from the borrowed download stream, which makes every type here
/// `'static`, and the future is boxed to prove that it is `Send` here.
fn extract_tar(
    mut rx: mpsc::Receiver<Bytes>,
    dst: PathBuf,
    fmt: TarBasedFmt,
) -> Pin<Box<dyn Future<Output = io::Result<ExtractedFiles>> + Send>> {
    Box::pin(async move {
        let stream = stream::poll_fn(move |cx| rx.poll_recv(cx).map(|bytes| bytes.map(Ok)));
        let archive = StaticTarArchive(
            open_tar_archive(create_tar_decoder(stream, fmt))
                .await
                .map_err(io::Error::from)?,
        );

        let extracted_files = Arc::new(Mutex::new(ExtractedFiles::new()));

        ExtractedFilesRecorder {
            inner: archive,
            extracted_files: extracted_files.clone(),
        }
        .extract_in(
            dst,
            ExtractPolicy::default()
                .link_policy(LinkPolicy::default().symlink_policy(SymlinkPolicy::Skip)),
        )
        .await
        .map_err(|err| io::Error::from(extract_error(err)))?;

        Ok(Arc::into_inner(extracted_files)
            .expect("the recorder is dropped by extract_in")
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner))
    })
}

#[cfg(test)]
pub(super) mod test {
    use super::*;

    use std::{collections::HashSet, ffi::OsStr, fs};

    use tempfile::tempdir;

    use crate::download::ExtractedFilesEntry;

    #[test]
    fn test_normalize_member_path() {
        for (path, expected) in [
            ("a", Some("a")),
            ("./a/b", Some("a/b")),
            ("a//b/", Some("a/b")),
            ("a/./b/.", Some("a/b")),
            ("./", Some("")),
            ("", Some("")),
            ("a/../b", Some("b")),
            ("/a", None),
            ("../a", None),
            ("a/../../b", None),
        ] {
            assert_eq!(normalize_member_path(path).as_deref(), expected, "{path:?}");
        }
    }

    /// Minimal ustar archive writer for tests.
    #[derive(Default)]
    pub(crate) struct TarBuilder(Vec<u8>);

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

        pub(crate) fn file(mut self, name: &str, mode: u32, data: &[u8]) -> Self {
            self.header(name, b'0', mode, data.len(), "");
            self.data(data);
            self
        }

        /// Add a regular file using a legacy v7 header.
        pub(crate) fn v7_file(mut self, name: &str, data: &[u8]) -> Self {
            self.header(name, 0, 0o755, data.len(), "");
            let start = self.0.len() - 512;
            let header = &mut self.0[start..];
            header[257..265].fill(0);
            header[148..156].copy_from_slice(b"        ");
            let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
            header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
            self.data(data);
            self
        }

        pub(crate) fn entry(mut self, name: &str, typeflag: u8, link: &str) -> Self {
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

        pub(crate) fn finish(mut self) -> Bytes {
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
            .file("dir/../file", 0o644, b"file")
            .file("/abs", 0o644, b"abs")
            .file("../escape", 0o644, b"escape")
            .file("dir/../../escape2", 0o644, b"escape")
            .entry("symlink", b'2', "../escape")
            .entry("hardlink", b'1', "file")
            .entry("fifo", b'6', "")
            .finish();

        let extracted_files = extract(archive, &dst).await.unwrap();

        assert_eq!(
            extracted_files.0,
            [
                (Path::new(".").into(), dir_entry(&["dir", "file"])),
                (Path::new("dir").into(), dir_entry(&["bin"])),
                (Path::new("dir/bin").into(), ExtractedFilesEntry::File),
                (Path::new("file").into(), ExtractedFilesEntry::File),
            ]
            .into_iter()
            .collect()
        );

        assert_eq!(fs::read(dst.join("dir/bin")).unwrap(), b"bin");
        assert_eq!(fs::read(dst.join("file")).unwrap(), b"file");
        // Absolute paths are skipped, as in zip extraction.
        assert!(dst.join("abs").symlink_metadata().is_err());
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
            assert_eq!(mode("file") & 0o111, 0);
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
    async fn v7_archive() {
        let tempdir = tempdir().unwrap();

        let archive = TarBuilder::default()
            .v7_file("a", b"a")
            .v7_file("dir/b", b"b")
            .finish();
        let dst = tempdir.path().join("v7");
        let extracted_files = extract(archive, &dst).await.unwrap();
        assert!(extracted_files.has_file(Path::new("a")));
        assert!(extracted_files.has_file(Path::new("dir/b")));
        assert_eq!(fs::read(dst.join("dir/b")).unwrap(), b"b");

        // Mixing v7 with ustar/pax/GNU headers is rejected, since there is no
        // way to tell how other implementations would interpret it.
        for archive in [
            TarBuilder::default()
                .v7_file("a", b"a")
                .file("b", 0o644, b"b")
                .finish(),
            TarBuilder::default()
                .v7_file("a", b"a")
                .pax(&[("path", "c")])
                .file("b", 0o644, b"b")
                .finish(),
        ] {
            let dst = tempdir.path().join("mixed");
            let err = extract(archive, &dst).await.unwrap_err();
            assert!(err.to_string().contains("v7"), "{err}");
            assert!(!dst.join("b").exists());
            assert!(!dst.join("c").exists());
        }
    }

    /// cargo-quickinstall builds its tarballs with `tar --format=v7`, using
    /// GNU tar on Linux and bsdtar on macOS. The two encode v7 headers
    /// differently (e.g. bsdtar marks directories only with a trailing `/`),
    /// so test both. The fixtures were generated with:
    ///
    /// ```sh
    /// printf '#!/bin/sh\necho foo\n' > cargo-foo && chmod 755 cargo-foo
    /// mkdir share && head -c 1500 /dev/zero | tr '\0' x > share/doc.txt
    /// ln -s cargo-foo cargo-foo-link
    /// tar --format=v7 --owner=0 --group=0 --numeric-owner --sort=name \
    ///     -cf - cargo-foo cargo-foo-link share | gzip -9n > gnu-v7.tar.gz
    /// bsdtar --format=v7 --uid 0 --gid 0 \
    ///     -cf - cargo-foo cargo-foo-link share | gzip -9n > bsdtar-v7.tar.gz
    /// ```
    #[tokio::test]
    async fn quickinstall_v7_archives() {
        let tempdir = tempdir().unwrap();

        for (name, archive) in [
            ("gnu", &include_bytes!("tar/testdata/gnu-v7.tar.gz")[..]),
            (
                "bsdtar",
                &include_bytes!("tar/testdata/bsdtar-v7.tar.gz")[..],
            ),
        ] {
            let dst = tempdir.path().join(name);
            let extracted_files = extract_tar_based_stream(
                stream::iter([Ok(Bytes::from_static(archive))]),
                &dst,
                TarBasedFmt::Tgz,
            )
            .await
            .unwrap_or_else(|err| panic!("{name}: {err}"));

            assert_eq!(
                extracted_files.0,
                [
                    (Path::new(".").into(), dir_entry(&["cargo-foo", "share"])),
                    (Path::new("cargo-foo").into(), ExtractedFilesEntry::File),
                    (Path::new("share").into(), dir_entry(&["doc.txt"])),
                    (Path::new("share/doc.txt").into(), ExtractedFilesEntry::File),
                ]
                .into_iter()
                .collect(),
                "{name}"
            );

            assert_eq!(
                fs::read(dst.join("cargo-foo")).unwrap(),
                b"#!/bin/sh\necho foo\n",
                "{name}"
            );
            assert_eq!(
                fs::read(dst.join("share/doc.txt")).unwrap(),
                [b'x'; 1500],
                "{name}"
            );
            // Symlinks are skipped.
            assert!(
                dst.join("cargo-foo-link").symlink_metadata().is_err(),
                "{name}"
            );

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                let mode = |path| fs::metadata(dst.join(path)).unwrap().permissions().mode();
                assert_ne!(mode("cargo-foo") & 0o111, 0, "{name}");
                assert_eq!(mode("share/doc.txt") & 0o111, 0, "{name}");
            }
        }
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
