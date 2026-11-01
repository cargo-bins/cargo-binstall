//! Zip extraction, backed by Astral's hardened fork of `async_zip`.
//!
//! The central directory is authoritative: entries are only read through it,
//! and `async_zip`'s seekable reader rejects archives that different zip
//! implementations could interpret differently (central directory not bound
//! to the end record, trailing data, overlapping entries, local file headers
//! disagreeing with the central directory, zip-in-comment, ...).
//!
//! On top of that, every entry's CRC32 and sizes are verified after
//! extraction, so a crafted archive can't smuggle bytes past them.

use std::{
    fs::{create_dir_all, File},
    io::{self, BufReader, Write},
    path::Path,
};

use async_zip::{base::read::seek::ZipFileReader, StoredZipEntry};
use futures_lite::{future::block_on, io::AsyncReadExt};
use futures_util::io::AllowStdIo;
use normalize_path::NormalizePath;
use tracing::warn;

use super::{DownloadError, ExtractedFiles};

const MAX_LINK_TARGET: u64 = 4096;
const COPY_BUF_SIZE: usize = 64 * 1024;

const S_IFMT: u16 = 0o170_000;
const S_IFDIR: u16 = 0o040_000;
const S_IFLNK: u16 = 0o120_000;

fn create_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .expect("all full entry paths should have parent paths");
    create_dir_all(parent)
}

fn invalid_data(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Reject traversal and absolute names, same rules as rc-zip used to apply.
fn sanitized_name(name: &str) -> Option<&str> {
    if name.contains("..") {
        return None;
    }

    if cfg!(windows) {
        if name.contains(":\\") || name.starts_with('\\') {
            return None;
        }
        Some(name)
    } else {
        // strip absolute prefix on entries pointing to root path
        Some(name.trim_start_matches('/'))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    File,
    Directory,
    Symlink,
}

fn entry_kind(entry: &StoredZipEntry, name: &str) -> EntryKind {
    let mode = entry.unix_permissions().unwrap_or(0) & S_IFMT;

    if mode == S_IFLNK {
        EntryKind::Symlink
    } else if mode == S_IFDIR || name.ends_with('/') {
        EntryKind::Directory
    } else {
        EntryKind::File
    }
}

type Reader = ZipFileReader<AllowStdIo<BufReader<File>>>;

/// Stream the entry at `index` into `writer`, then verify its CRC32 and sizes
/// against the central directory.
async fn copy_entry(
    zip: &mut Reader,
    index: usize,
    name: &str,
    writer: &mut impl Write,
) -> Result<(), DownloadError> {
    let entry = &zip.file().entries()[index];
    let (crc32, size, compressed_size) = (
        entry.crc32(),
        entry.uncompressed_size(),
        entry.compressed_size(),
    );

    let mut reader = zip.reader_without_entry(index).await?;
    let mut buf = vec![0; COPY_BUF_SIZE];
    let mut written = 0;

    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > size {
            return Err(invalid_data(format!(
                "zip entry {name} is larger than its declared size {size}"
            ))
            .into());
        }
        writer.write_all(&buf[..n])?;
    }

    if written != size {
        return Err(invalid_data(format!(
            "zip entry {name}: declared size {size}, got {written}"
        ))
        .into());
    }

    let consumed = reader.bytes_read();
    if consumed != compressed_size {
        return Err(invalid_data(format!(
            "zip entry {name}: declared compressed size {compressed_size}, consumed {consumed}"
        ))
        .into());
    }

    let actual = reader.compute_hash();
    if actual != crc32 {
        return Err(invalid_data(format!(
            "zip entry {name}: crc32 mismatch, expected {crc32:#010x}, got {actual:#010x}"
        ))
        .into());
    }

    Ok(())
}

pub(super) fn do_extract_zip(f: File, dir: &Path) -> Result<ExtractedFiles, DownloadError> {
    block_on(extract_zip(f, dir))
}

async fn extract_zip(f: File, dir: &Path) -> Result<ExtractedFiles, DownloadError> {
    let mut extracted_files = ExtractedFiles::new();

    let mut zip = ZipFileReader::new(AllowStdIo::new(BufReader::new(f))).await?;

    for index in 0..zip.file().entries().len() {
        let entry = &zip.file().entries()[index];

        let Ok(raw_name) = entry.filename().as_str() else {
            warn!("Skip zip entry with non-UTF-8 name, report to packager please");
            continue;
        };
        let Some(name) = sanitized_name(raw_name) else {
            warn!("Skip zip entry {raw_name} for suspected zip slip");
            continue;
        };
        let kind = entry_kind(entry, name);
        let Some(name) = &Path::new(name).try_normalize() else {
            warn!("Skip zip entry {name} pointing outside, report to packager please");
            continue;
        };
        let raw_name = raw_name.to_owned();
        let path = &dir.join(name);

        match kind {
            #[cfg(any(unix, target_os = "wasi"))]
            EntryKind::Symlink => {
                use std::ffi::OsStr;

                #[cfg(unix)]
                use std::os::unix::{ffi::OsStrExt, fs::symlink};

                #[cfg(target_os = "wasi")]
                use std::os::wasi::{ffi::OsStrExt, fs::symlink_path as symlink};

                let size = entry.uncompressed_size();
                let mut src = Vec::new();
                if (1..=MAX_LINK_TARGET).contains(&size) {
                    copy_entry(&mut zip, index, &raw_name, &mut src).await?;
                }
                if src.is_empty() || src.contains(&0) {
                    warn!(
                        "Skip zip symlink dest={} with invalid target",
                        path.display(),
                    );
                    continue;
                }

                let src = Path::new(OsStr::from_bytes(&src));
                let Some(src) = &src.try_normalize() else {
                    warn!(
                        "Skip zip symlink {} => {} with target pointing outside, beware of possible malware",
                        src.display(),
                        path.display(),
                    );
                    continue;
                };

                // src is relative to link_dir
                let link_dir = name.parent().unwrap_or(Path::new(""));
                if link_dir.join(src) == *name {
                    warn!("Skip symlink loop {}", path.display());
                    continue;
                }

                create_parent_dir(path)?;
                symlink(src, path)?;

                extracted_files.add_file(name);
            }
            EntryKind::Directory => {
                create_dir_all(path)?;
            }
            #[cfg_attr(any(unix, target_os = "wasi"), allow(unreachable_patterns))]
            EntryKind::File | EntryKind::Symlink => {
                create_parent_dir(path)?;

                let mut entry_writer = File::create_new(path)?;
                copy_entry(&mut zip, index, &raw_name, &mut entry_writer).await?;

                extracted_files.add_file(name);
            }
        }
    }

    Ok(extracted_files)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Seek, Write},
        path::{Path, PathBuf},
    };

    use tempfile::TempDir;

    use super::*;

    const MODE_FILE: u32 = 0o100_644;
    const MODE_DIR: u32 = 0o040_755;
    const MODE_SYMLINK: u32 = 0o120_777;

    struct TestEntry {
        name: Vec<u8>,
        mode: u32,
        data: Vec<u8>,
        /// Overrides the computed CRC32 in both headers.
        crc: Option<u32>,
        /// Overrides the name stored in the local file header.
        local_name: Option<Vec<u8>>,
    }

    impl TestEntry {
        fn file(name: &str, data: &[u8]) -> Self {
            Self {
                name: name.into(),
                mode: MODE_FILE,
                data: data.to_vec(),
                crc: None,
                local_name: None,
            }
        }

        fn dir(name: &str) -> Self {
            Self {
                name: format!("{}/", name.trim_end_matches('/')).into(),
                mode: MODE_DIR,
                data: Vec::new(),
                crc: None,
                local_name: None,
            }
        }

        /// A zip symlink entry stores the link target as its content.
        fn symlink(name: &str, target: impl AsRef<[u8]>) -> Self {
            Self {
                name: name.into(),
                mode: MODE_SYMLINK,
                data: target.as_ref().to_vec(),
                crc: None,
                local_name: None,
            }
        }
    }

    /// Hand-rolled, STORE-only zip writer so the tests need no extra dependency
    /// and can produce entries that real archivers refuse to create.
    fn build_zip(entries: &[TestEntry]) -> Vec<u8> {
        const DOS_DATE: u16 = (1 << 5) | 1; // 1980-01-01
        const UTF8_FLAG: u16 = 1 << 11;

        let mut out = Vec::new();
        let mut central = Vec::new();

        for e in entries {
            let offset = u32::try_from(out.len()).unwrap();
            let mut crc = flate2::Crc::new();
            crc.update(&e.data);
            let crc = e.crc.unwrap_or(crc.sum());
            let size = u32::try_from(e.data.len()).unwrap();
            let name_len = u16::try_from(e.name.len()).unwrap();
            let local_name = e.local_name.as_ref().unwrap_or(&e.name);
            let local_name_len = u16::try_from(local_name.len()).unwrap();

            // Local file header
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend(20u16.to_le_bytes()); // version needed
            out.extend(UTF8_FLAG.to_le_bytes());
            out.extend(0u16.to_le_bytes()); // method: stored
            out.extend(0u16.to_le_bytes()); // time
            out.extend(DOS_DATE.to_le_bytes());
            out.extend(crc.to_le_bytes());
            out.extend(size.to_le_bytes()); // compressed size
            out.extend(size.to_le_bytes()); // uncompressed size
            out.extend(local_name_len.to_le_bytes());
            out.extend(0u16.to_le_bytes()); // extra len
            out.extend(local_name);
            out.extend(&e.data);

            // Central directory header
            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend(((3u16 << 8) | 20).to_le_bytes()); // made by: Unix
            central.extend(20u16.to_le_bytes()); // version needed
            central.extend(UTF8_FLAG.to_le_bytes());
            central.extend(0u16.to_le_bytes()); // method: stored
            central.extend(0u16.to_le_bytes()); // time
            central.extend(DOS_DATE.to_le_bytes());
            central.extend(crc.to_le_bytes());
            central.extend(size.to_le_bytes());
            central.extend(size.to_le_bytes());
            central.extend(name_len.to_le_bytes());
            central.extend(0u16.to_le_bytes()); // extra len
            central.extend(0u16.to_le_bytes()); // comment len
            central.extend(0u16.to_le_bytes()); // disk number
            central.extend(0u16.to_le_bytes()); // internal attrs
            central.extend((e.mode << 16).to_le_bytes()); // external attrs: unix mode
            central.extend(offset.to_le_bytes());
            central.extend(&e.name);
        }

        let cd_offset = u32::try_from(out.len()).unwrap();
        let cd_size = u32::try_from(central.len()).unwrap();
        let count = u16::try_from(entries.len()).unwrap();
        out.extend(central);

        // End of central directory
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(count.to_le_bytes());
        out.extend(cd_size.to_le_bytes());
        out.extend(cd_offset.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }

    /// `<root>/out` is the extraction dir; anything else under `<root>` means
    /// the extraction escaped.
    struct Sandbox {
        root: TempDir,
    }

    impl Sandbox {
        fn new() -> Self {
            Self {
                root: TempDir::new().unwrap(),
            }
        }

        fn out(&self) -> PathBuf {
            self.root.path().join("out")
        }

        fn extract(&self, entries: &[TestEntry]) -> Result<ExtractedFiles, DownloadError> {
            self.extract_raw(&build_zip(entries))
        }

        fn extract_raw(&self, zip: &[u8]) -> Result<ExtractedFiles, DownloadError> {
            let mut f = tempfile::tempfile().unwrap();
            f.write_all(zip).unwrap();
            f.rewind().unwrap();

            let out = self.out();
            fs::create_dir_all(&out).unwrap();
            do_extract_zip(f, &out)
        }

        fn assert_root_contains_only(&self, expected: &[&str]) {
            let mut names: Vec<String> = fs::read_dir(self.root.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();

            let mut expected = expected.to_vec();
            expected.sort();

            assert_eq!(
                names, expected,
                "nothing may be created outside the extraction dir"
            );
        }
    }

    #[test]
    fn extracts_regular_files_and_dirs() {
        let sb = Sandbox::new();
        let files = sb
            .extract(&[
                TestEntry::dir("d"),
                TestEntry::file("d/a.txt", b"a"),
                TestEntry::file("b.txt", b"b"),
            ])
            .unwrap();

        assert_eq!(fs::read(sb.out().join("d/a.txt")).unwrap(), b"a");
        assert_eq!(fs::read(sb.out().join("b.txt")).unwrap(), b"b");
        assert!(files.has_file(Path::new("d/a.txt")));
        assert!(files.has_file(Path::new("b.txt")));
    }

    #[test]
    fn entries_escaping_the_extraction_dir_are_skipped() {
        let sb = Sandbox::new();
        sb.extract(&[
            TestEntry::file("../escape1.txt", b"x"),
            TestEntry::file("a/../../escape2.txt", b"x"),
            TestEntry::file("ok.txt", b"ok"),
        ])
        .unwrap();

        sb.assert_root_contains_only(&["out"]);
        assert!(sb.out().join("ok.txt").is_file());
    }

    #[test]
    fn rooted_entry_names_stay_inside() {
        let sb = Sandbox::new();
        sb.extract(&[
            TestEntry::file("/escape/rooted.txt", b"x"),
            TestEntry::file("ok.txt", b"ok"),
        ])
        .unwrap();

        // Depending on the platform the rooted entry is either skipped or
        // extracted relative to the extraction dir, but it never leaves it.
        sb.assert_root_contains_only(&["out"]);
        assert!(sb.out().join("ok.txt").is_file());
    }

    #[test]
    fn dotdot_inside_entry_name_never_escapes() {
        let sb = Sandbox::new();
        // rc-zip's `sanitized_name()` drops names containing `..`, so this
        // entry is skipped before our own normalization sees it.
        sb.extract(&[TestEntry::file("a/../b.txt", b"b")]).unwrap();

        sb.assert_root_contains_only(&["out"]);
    }

    #[test]
    fn duplicate_file_entries_are_an_error() {
        let sb = Sandbox::new();
        let res = sb.extract(&[TestEntry::file("x", b"1"), TestEntry::file("x", b"2")]);

        assert!(res.is_err());
        assert_eq!(fs::read(sb.out().join("x")).unwrap(), b"1");
    }

    // Parser differentials (CVE-2025-54368 class): archives that zip
    // implementations disagree on must be rejected rather than extracted.

    #[test]
    fn prepended_data_is_rejected() {
        // rc-zip used to silently re-base the central directory here, while
        // parsers trusting the recorded offset see something else.
        let mut zip = b"garbage".to_vec();
        zip.extend(build_zip(&[TestEntry::file("a.txt", b"a")]));

        let sb = Sandbox::new();
        assert!(sb.extract_raw(&zip).is_err());
        assert!(!sb.out().join("a.txt").exists());
    }

    #[test]
    fn trailing_data_is_rejected() {
        let mut zip = build_zip(&[TestEntry::file("a.txt", b"a")]);
        zip.extend(b"PK\x05\x06 trailing");

        let sb = Sandbox::new();
        assert!(sb.extract_raw(&zip).is_err());
    }

    #[test]
    fn trailing_nul_padding_is_allowed() {
        let mut zip = build_zip(&[TestEntry::file("a.txt", b"a")]);
        zip.extend([0; 16]);

        let sb = Sandbox::new();
        sb.extract_raw(&zip).unwrap();
        assert_eq!(fs::read(sb.out().join("a.txt")).unwrap(), b"a");
    }

    #[test]
    fn appended_zip_is_rejected() {
        // A second archive concatenated after the first: some tools see
        // the first one, others the last one.
        let mut zip = build_zip(&[TestEntry::file("benign.txt", b"a")]);
        zip.extend(build_zip(&[TestEntry::file("evil.txt", b"b")]));

        let sb = Sandbox::new();
        assert!(sb.extract_raw(&zip).is_err());
        sb.assert_root_contains_only(&["out"]);
        assert!(fs::read_dir(sb.out()).unwrap().next().is_none());
    }

    #[test]
    fn crc_mismatch_is_rejected() {
        let sb = Sandbox::new();
        let mut entry = TestEntry::file("a.txt", b"a");
        entry.crc = Some(0xdead_beef);
        assert!(sb.extract(&[entry]).is_err());
    }

    #[test]
    fn zero_crc_is_still_checked() {
        // rc-zip skipped the check when the declared CRC32 was 0.
        let sb = Sandbox::new();
        let mut entry = TestEntry::file("a.txt", b"a");
        entry.crc = Some(0);
        assert!(sb.extract(&[entry]).is_err());
    }

    #[test]
    fn local_header_name_mismatch_is_rejected() {
        let sb = Sandbox::new();
        let mut entry = TestEntry::file("benign.txt", b"a");
        entry.local_name = Some(b"evil.txt".to_vec());
        assert!(sb.extract(&[entry]).is_err());
        assert!(!sb.out().join("evil.txt").exists());
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::ffi::OsStrExt;

        use super::*;

        fn exists_nofollow(path: &Path) -> bool {
            fs::symlink_metadata(path).is_ok()
        }

        #[test]
        fn absolute_target_is_skipped_and_cannot_be_written_through() {
            let sb = Sandbox::new();
            let victim = sb.root.path().join("victim");
            fs::write(&victim, b"original").unwrap();

            sb.extract(&[
                TestEntry::symlink("link", victim.as_os_str().as_bytes()),
                TestEntry::file("link", b"PWNED"),
            ])
            .unwrap();

            assert_eq!(fs::read(&victim).unwrap(), b"original");
            sb.assert_root_contains_only(&["out", "victim"]);

            // The symlink was skipped, so the file entry is just a regular file.
            let meta = fs::symlink_metadata(sb.out().join("link")).unwrap();
            assert!(meta.is_file());
        }

        #[test]
        fn target_pointing_outside_is_skipped() {
            let sb = Sandbox::new();
            sb.extract(&[
                TestEntry::symlink("l", "../victim"),
                TestEntry::symlink("a/l", "../../victim"),
                TestEntry::symlink("b/l", "x/../../victim"),
            ])
            .unwrap();

            sb.assert_root_contains_only(&["out"]);
            assert!(!exists_nofollow(&sb.out().join("l")));
            assert!(!exists_nofollow(&sb.out().join("a/l")));
            assert!(!exists_nofollow(&sb.out().join("b/l")));
        }

        #[test]
        fn chained_symlinks_cannot_escape() {
            let sb = Sandbox::new();
            sb.extract(&[
                TestEntry::symlink("a/u", ".."),
                TestEntry::symlink("a/u/l", "../../x"),
                TestEntry::file("a/u/f", b"x"),
            ])
            .unwrap();

            sb.assert_root_contains_only(&["out"]);
        }

        #[test]
        fn valid_relative_symlink_is_created_relative_to_its_dir() {
            let sb = Sandbox::new();
            let files = sb
                .extract(&[
                    TestEntry::file("bin/foo-1.0", b"binary"),
                    TestEntry::symlink("bin/foo", "foo-1.0"),
                ])
                .unwrap();

            let link = sb.out().join("bin/foo");
            assert_eq!(fs::read_link(&link).unwrap(), Path::new("foo-1.0"));
            assert_eq!(fs::read(&link).unwrap(), b"binary");
            assert!(files.has_file(Path::new("bin/foo")));
        }

        #[test]
        fn dangling_symlink_is_allowed() {
            let sb = Sandbox::new();
            let files = sb.extract(&[TestEntry::symlink("l", "missing")]).unwrap();

            let link = sb.out().join("l");
            assert_eq!(fs::read_link(&link).unwrap(), Path::new("missing"));
            assert!(files.has_file(Path::new("l")));
        }

        #[test]
        fn symlink_target_is_normalized() {
            let sb = Sandbox::new();
            sb.extract(&[
                TestEntry::symlink("m", "a/../b"),
                // "." used to normalize to an empty path, which `symlink` rejects.
                TestEntry::symlink("n", "."),
            ])
            .unwrap();

            assert_eq!(fs::read_link(sb.out().join("m")).unwrap(), Path::new("b"));
            assert_eq!(fs::read_link(sb.out().join("n")).unwrap(), Path::new("."));
        }

        #[test]
        fn write_through_directory_symlink_stays_inside() {
            let sb = Sandbox::new();
            sb.extract(&[
                TestEntry::dir("real"),
                TestEntry::symlink("d", "real"),
                TestEntry::file("d/x.txt", b"x"),
            ])
            .unwrap();

            assert_eq!(
                fs::read_link(sb.out().join("d")).unwrap(),
                Path::new("real")
            );
            assert_eq!(fs::read(sb.out().join("real/x.txt")).unwrap(), b"x");
            sb.assert_root_contains_only(&["out"]);
        }

        #[test]
        fn write_through_dangling_directory_symlink_fails() {
            let sb = Sandbox::new();
            let res = sb.extract(&[
                TestEntry::symlink("d", "missing"),
                TestEntry::file("d/x.txt", b"x"),
            ]);

            assert!(res.is_err());
            assert!(!sb.out().join("missing").exists());
            sb.assert_root_contains_only(&["out"]);
        }

        #[test]
        fn file_entry_cannot_overwrite_symlink_or_its_target() {
            let sb = Sandbox::new();
            let res = sb.extract(&[
                TestEntry::file("target", b"A"),
                TestEntry::symlink("l", "target"),
                TestEntry::file("l", b"B"),
            ]);

            assert!(res.is_err());
            assert_eq!(fs::read(sb.out().join("target")).unwrap(), b"A");
        }

        #[test]
        fn symlink_loops_are_skipped() {
            let sb = Sandbox::new();
            sb.extract(&[
                TestEntry::symlink("l", "l"),
                TestEntry::symlink("a/l", "l"),
                TestEntry::file("ok.txt", b"ok"),
            ])
            .unwrap();

            assert!(!exists_nofollow(&sb.out().join("l")));
            assert!(!exists_nofollow(&sb.out().join("a/l")));
            assert!(sb.out().join("ok.txt").is_file());
        }

        #[test]
        fn invalid_targets_are_skipped_without_failing_extraction() {
            let sb = Sandbox::new();
            sb.extract(&[
                TestEntry::symlink("empty", b""),
                TestEntry::symlink("nul", b"a\0b"),
                TestEntry::symlink("long", vec![b'a'; 4097]),
                TestEntry::file("ok.txt", b"ok"),
            ])
            .unwrap();

            for name in ["empty", "nul", "long"] {
                assert!(!exists_nofollow(&sb.out().join(name)), "{name}");
            }
            assert!(sb.out().join("ok.txt").is_file());
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn non_utf8_target_is_supported() {
            use std::{ffi::OsString, os::unix::ffi::OsStringExt};

            let sb = Sandbox::new();
            let target = vec![0xff, b'x'];
            sb.extract(&[TestEntry::symlink("l", &target)]).unwrap();

            assert_eq!(
                fs::read_link(sb.out().join("l")).unwrap(),
                PathBuf::from(OsString::from_vec(target))
            );
        }
    }
}
