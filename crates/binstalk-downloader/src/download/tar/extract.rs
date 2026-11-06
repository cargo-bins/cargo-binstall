//! Extraction of tar archives.

use std::{
    path::{Component, Path},
    sync::{Arc, Mutex, PoisonError},
};

use bytes::Bytes;
use futures_util::Stream;
use normalize_path::NormalizePath;
use tar_codec::{
    extract::{ExtractPolicy, LinkPolicy, SymlinkPolicy},
    Archive, ExtractError, Member, MemberMetadata,
};
use tracing::debug;

use super::{
    decode_error, DownloadError, ExtractedFiles, TarArchive, TarBasedFmt, TarDecodeError, TarError,
    TarErrorInner,
};
use crate::utils::extract_with_task;

pub(in crate::download) async fn extract_tar_based_stream<S>(
    stream: S,
    dst: &Path,
    fmt: TarBasedFmt,
) -> Result<ExtractedFiles, DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin,
{
    debug!("Extracting from {fmt} archive to {}", dst.display());

    extract_with_task(stream, |rx| async move {
        let mut archive = TarArchive::open(rx, fmt).await?;

        // `extract_in` consumes the archive, so the files are recorded in a
        // shared `ExtractedFiles`.
        let extracted_files = Arc::new(Mutex::new(ExtractedFiles::new()));
        archive.extracted_files = Some(extracted_files.clone());

        archive
            .extract_in(
                dst,
                ExtractPolicy::default()
                    .link_policy(LinkPolicy::default().symlink_policy(SymlinkPolicy::Skip)),
            )
            .await
            .map_err(extract_error)?;

        Ok(Arc::into_inner(extracted_files)
            .expect("the archive is dropped by extract_in")
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner))
    })
    .await
}

fn extract_error(err: ExtractError<TarDecodeError>) -> DownloadError {
    match err {
        ExtractError::Archive(err) => decode_error(err),
        err => DownloadError::Tar(TarError(TarErrorInner::Extract(err))),
    }
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

/// Only keep regular files and directories, with their paths normalized so
/// that the paths recorded in `extracted_files` are exactly the ones
/// extracted, and replace every other member with a no-op.
pub(super) fn filter_member<P>(
    extracted_files: &Mutex<ExtractedFiles>,
    member: Member<P>,
) -> Member<P> {
    let mut extracted_files = extracted_files
        .lock()
        .unwrap_or_else(PoisonError::into_inner);

    match member {
        Member::File {
            metadata,
            size,
            executable,
            payload,
        } => match normalize_member_path(&metadata.path) {
            Some(path) if !path.is_empty() => {
                extracted_files.add_file(Path::new(&path));
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
                extracted_files.add_dir(Path::new(if path.is_empty() { "." } else { &path }));
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
    }
}

#[cfg(test)]
mod test {
    use super::{super::test::TarBuilder, *};

    use std::{collections::HashSet, ffi::OsStr, fs};

    use futures_util::stream;
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
            ("gnu", &include_bytes!("testdata/gnu-v7.tar.gz")[..]),
            ("bsdtar", &include_bytes!("testdata/bsdtar-v7.tar.gz")[..]),
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
