use std::{
    borrow::Cow,
    fs::{File, create_dir_all},
    io::{self, BufReader, Read},
    path::{Component, Path, PathBuf},
};

use sevenz_rust2::{ArchiveEntry, ArchiveReader, Password};
use tracing::warn;

use super::{DownloadError, ExtractedFiles, archive_entry::create_parent_dir};

/// The most memory the decoder of a 7z archive may ask for, in KiB, for its
/// header buffers and, separately, for its dictionaries and models.
///
/// The largest dictionary 7-Zip itself uses on 64-bit is 1.5 GiB (`-mx9`
/// with `-md=1536m`); its `-mx9` default is 64 MiB. Without a limit a
/// malformed archive could make the decoder allocate up to 4 GiB.
const MEMORY_LIMIT_KIB: usize = 1536 * 1024;

/// Set in `windows_attributes` by 7-Zip/p7zip when the high 16 bits hold the
/// entry's unix `st_mode`.
#[cfg_attr(not(any(unix, target_os = "wasi")), allow(dead_code))]
const FILE_ATTRIBUTE_UNIX_EXTENSION: u32 = 0x8000;

/// The unix mode of `entry`, if the archive recorded one.
#[cfg(any(unix, target_os = "wasi"))]
fn unix_mode(entry: &ArchiveEntry) -> Option<u32> {
    let attrs = entry.windows_attributes;
    (entry.has_windows_attributes && attrs & FILE_ATTRIBUTE_UNIX_EXTENSION != 0)
        .then_some(attrs >> 16)
}

#[cfg(any(unix, target_os = "wasi"))]
fn is_symlink(entry: &ArchiveEntry) -> bool {
    const S_IFMT: u32 = 0o170000;
    const S_IFLNK: u32 = 0o120000;

    unix_mode(entry).is_some_and(|mode| mode & S_IFMT == S_IFLNK)
}

/// The path of the entry `name`, relative to the extraction dir, or `None` if
/// it is empty or would not stay inside of it.
fn entry_path(name: &str) -> Option<PathBuf> {
    // 7-Zip stores `/` as the separator, but treat a `\` from an archive
    // written by another tool on Windows as one too, so that `..\x` is caught
    // on every platform.
    let name = name.replace('\\', "/");

    let mut path = PathBuf::new();
    for component in Path::new(&name).components() {
        match component {
            // `a:b` would create the alternate data stream `b` of `a`.
            #[cfg(windows)]
            Component::Normal(part) if part.as_encoded_bytes().contains(&b':') => return None,
            Component::Normal(part) => path.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    (!path.as_os_str().is_empty()).then_some(path)
}

fn extract_entry(
    entry: &ArchiveEntry,
    data: &mut dyn Read,
    dir: &Path,
    extracted_files: &mut ExtractedFiles,
) -> io::Result<()> {
    if entry.is_anti_item {
        // Marks a file deleted by an update of the archive, there's nothing
        // to extract.
        return Ok(());
    }

    let Some(name) = &entry_path(&entry.name) else {
        warn!(
            "Skip 7z entry {} pointing outside, report to packager please",
            entry.name
        );
        return Ok(());
    };
    let path = &dir.join(name);

    if entry.is_directory {
        return create_dir_all(path);
    }

    #[cfg(any(unix, target_os = "wasi"))]
    if is_symlink(entry) {
        return super::archive_entry::extract_symlink("7z", name, path, data, extracted_files);
    }

    create_parent_dir(path)?;

    let mut entry_writer = File::create_new(path)?;
    io::copy(data, &mut entry_writer)?;

    extracted_files.add_file(name);

    Ok(())
}

pub(super) fn do_extract_7z(f: File, dir: &Path) -> Result<ExtractedFiles, DownloadError> {
    let mut archive = ArchiveReader::new_with_memory_limit(
        BufReader::new(f),
        Password::empty(),
        MEMORY_LIMIT_KIB,
    )?;

    let mut extracted_files = ExtractedFiles::new();
    // An error of ours, as opposed to one decoding the archive, which is
    // reported as-is rather than as a 7z error.
    let mut io_error = None;

    let res = archive.for_each_entries(|entry, data| {
        match extract_entry(entry, data, dir, &mut extracted_files) {
            Ok(()) => Ok(true),
            Err(err) => {
                io_error = Some(err);
                Err(sevenz_rust2::Error::Other(Cow::Borrowed(
                    "extraction of entry failed",
                )))
            }
        }
    });

    if let Some(err) = io_error {
        return Err(err.into());
    }
    res?;

    Ok(extracted_files)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Cursor, Seek, Write},
    };

    use sevenz_rust2::{ArchiveWriter, EncoderMethod, SourceReader};
    use tempfile::TempDir;

    use super::*;

    #[cfg_attr(not(unix), allow(dead_code))]
    enum TestEntry {
        File(&'static str, &'static [u8]),
        Dir(&'static str),
        Symlink(&'static str, &'static [u8]),
    }

    impl TestEntry {
        fn to_archive_entry(&self) -> (ArchiveEntry, Option<&'static [u8]>) {
            match *self {
                TestEntry::File(name, data) => (ArchiveEntry::new_file(name), Some(data)),
                TestEntry::Dir(name) => (ArchiveEntry::new_directory(name), None),
                TestEntry::Symlink(name, target) => {
                    let mut entry = ArchiveEntry::new_file(name);
                    entry.has_windows_attributes = true;
                    entry.windows_attributes = FILE_ATTRIBUTE_UNIX_EXTENSION | (0o120_777 << 16);
                    (entry, Some(target))
                }
            }
        }
    }

    /// Each entry is compressed on its own, as 7-Zip's `-ms=off` does.
    fn build_7z(entries: &[TestEntry]) -> Vec<u8> {
        let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        for entry in entries {
            let (entry, data) = entry.to_archive_entry();
            writer.push_archive_entry(entry, data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// All entries with data in one solid block, as 7-Zip does by default,
    /// with the filters 7-Zip uses for executables.
    fn build_solid_7z(entries: &[TestEntry]) -> Vec<u8> {
        let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        writer.set_content_methods(vec![
            EncoderMethod::BCJ_X86_FILTER.into(),
            EncoderMethod::LZMA2.into(),
        ]);

        let mut solid = Vec::new();
        let mut readers = Vec::new();
        for entry in entries {
            match entry.to_archive_entry() {
                (entry, Some(data)) => {
                    solid.push(entry);
                    readers.push(SourceReader::new(data));
                }
                (entry, None) => {
                    writer.push_archive_entry(entry, None::<&[u8]>).unwrap();
                }
            }
        }
        writer.push_archive_entries(solid, readers).unwrap();
        writer.finish().unwrap().into_inner()
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

        fn extract_bytes(&self, archive: &[u8]) -> Result<ExtractedFiles, DownloadError> {
            let mut f = tempfile::tempfile().unwrap();
            f.write_all(archive).unwrap();
            // Left at the end on purpose: that's where the downloader leaves it.
            f.flush().unwrap();

            let out = self.out();
            fs::create_dir_all(&out).unwrap();
            do_extract_7z(f, &out)
        }

        fn extract(&self, entries: &[TestEntry]) -> Result<ExtractedFiles, DownloadError> {
            self.extract_bytes(&build_7z(entries))
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
        for archive in [build_7z, build_solid_7z] {
            let sb = Sandbox::new();
            let files = sb
                .extract_bytes(&archive(&[
                    TestEntry::Dir("d"),
                    TestEntry::File("d/a.txt", b"a"),
                    TestEntry::File("b.txt", b"b"),
                    TestEntry::File("empty", b""),
                    TestEntry::Dir("empty-dir"),
                ]))
                .unwrap();

            assert_eq!(fs::read(sb.out().join("d/a.txt")).unwrap(), b"a");
            assert_eq!(fs::read(sb.out().join("b.txt")).unwrap(), b"b");
            assert_eq!(fs::read(sb.out().join("empty")).unwrap(), b"");
            assert!(sb.out().join("empty-dir").is_dir());
            assert!(files.has_file(Path::new("d/a.txt")));
            assert!(files.has_file(Path::new("b.txt")));
            assert!(files.has_file(Path::new("empty")));
        }
    }

    #[test]
    fn not_a_7z_archive_is_an_error() {
        let sb = Sandbox::new();
        let err = sb.extract_bytes(b"PK\x03\x04 not a 7z").unwrap_err();
        assert!(matches!(err, DownloadError::SevenZip(_)), "{err:?}");
    }

    #[test]
    fn corrupted_data_is_an_error() {
        let sb = Sandbox::new();
        let mut archive = build_7z(&[TestEntry::File("a.txt", b"some data to corrupt")]);
        // Packed data starts right after the 32-byte signature header.
        archive[32] ^= 0xff;

        assert!(sb.extract_bytes(&archive).is_err());
    }

    #[test]
    fn entries_escaping_the_extraction_dir_are_skipped() {
        let sb = Sandbox::new();
        sb.extract(&[
            TestEntry::File("../escape1.txt", b"x"),
            TestEntry::File("a/../../escape2.txt", b"x"),
            TestEntry::File("..\\escape3.txt", b"x"),
            TestEntry::File("/escape4.txt", b"x"),
            TestEntry::File("ok.txt", b"ok"),
        ])
        .unwrap();

        sb.assert_root_contains_only(&["out"]);
        assert_eq!(
            fs::read_dir(sb.out()).unwrap().count(),
            1,
            "only ok.txt is extracted"
        );
        assert!(sb.out().join("ok.txt").is_file());
    }

    #[test]
    fn backslash_is_a_separator() {
        let sb = Sandbox::new();
        let files = sb.extract(&[TestEntry::File("d\\a.txt", b"a")]).unwrap();

        assert_eq!(fs::read(sb.out().join("d/a.txt")).unwrap(), b"a");
        assert!(files.has_file(Path::new("d/a.txt")));
    }

    #[test]
    fn duplicate_file_entries_are_an_error() {
        let sb = Sandbox::new();
        let err = sb
            .extract(&[TestEntry::File("x", b"1"), TestEntry::File("x", b"2")])
            .unwrap_err();

        // Our own I/O error, not one about the archive.
        assert!(
            matches!(&err, DownloadError::Io(e) if e.kind() == io::ErrorKind::AlreadyExists),
            "{err:?}"
        );
        assert_eq!(fs::read(sb.out().join("x")).unwrap(), b"1");
    }

    #[test]
    fn file_left_at_any_position_is_read_from_the_start() {
        let sb = Sandbox::new();
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(&build_7z(&[TestEntry::File("a", b"a")]))
            .unwrap();
        f.seek(io::SeekFrom::Start(3)).unwrap();

        let out = sb.out();
        fs::create_dir_all(&out).unwrap();
        do_extract_7z(f, &out).unwrap();
        assert_eq!(fs::read(out.join("a")).unwrap(), b"a");
    }

    #[cfg(unix)]
    mod unix {
        use super::*;

        fn exists_nofollow(path: &Path) -> bool {
            fs::symlink_metadata(path).is_ok()
        }

        #[test]
        fn valid_relative_symlink_is_created_relative_to_its_dir() {
            for archive in [build_7z, build_solid_7z] {
                let sb = Sandbox::new();
                let files = sb
                    .extract_bytes(&archive(&[
                        TestEntry::File("bin/foo-1.0", b"binary"),
                        TestEntry::Symlink("bin/foo", b"foo-1.0"),
                    ]))
                    .unwrap();

                let link = sb.out().join("bin/foo");
                assert_eq!(fs::read_link(&link).unwrap(), Path::new("foo-1.0"));
                assert_eq!(fs::read(&link).unwrap(), b"binary");
                assert!(files.has_file(Path::new("bin/foo")));
            }
        }

        #[test]
        fn target_pointing_outside_is_skipped() {
            let sb = Sandbox::new();
            sb.extract(&[
                TestEntry::Symlink("l", b"../victim"),
                TestEntry::Symlink("a/l", b"../../victim"),
                TestEntry::Symlink("abs", b"/etc/passwd"),
            ])
            .unwrap();

            sb.assert_root_contains_only(&["out"]);
            assert!(!exists_nofollow(&sb.out().join("l")));
            assert!(!exists_nofollow(&sb.out().join("a/l")));
            assert!(!exists_nofollow(&sb.out().join("abs")));
        }

        #[test]
        fn file_entry_cannot_overwrite_symlink_or_its_target() {
            let sb = Sandbox::new();
            let res = sb.extract(&[
                TestEntry::File("target", b"A"),
                TestEntry::Symlink("l", b"target"),
                TestEntry::File("l", b"B"),
            ]);

            assert!(res.is_err());
            assert_eq!(fs::read(sb.out().join("target")).unwrap(), b"A");
        }

        #[test]
        fn file_with_unix_mode_is_not_a_symlink() {
            let sb = Sandbox::new();
            let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
            let mut entry = ArchiveEntry::new_file("exe");
            entry.has_windows_attributes = true;
            entry.windows_attributes = FILE_ATTRIBUTE_UNIX_EXTENSION | (0o100_755 << 16);
            writer
                .push_archive_entry(entry, Some(&b"binary"[..]))
                .unwrap();
            let archive = writer.finish().unwrap().into_inner();

            sb.extract_bytes(&archive).unwrap();
            let meta = fs::symlink_metadata(sb.out().join("exe")).unwrap();
            assert!(meta.is_file());
        }
    }
}
