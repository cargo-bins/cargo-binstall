//! Streaming zip extraction, backed by Astral's hardened fork of `async_zip`.
//!
//! Entries are extracted from their local file headers while the archive is
//! still being downloaded. Once the central directory is reached, every entry
//! is reconciled against it, the same way uv does since CVE-2025-54368:
//! names, CRC32 and sizes must agree, every local file header must have a
//! central directory entry and vice versa, the entry count must match, and no
//! data may follow the archive (`async_zip` also rejects zip-in-comment and
//! central directories not bound to the end record). Any disagreement is an
//! error and the files extracted so far are removed.
//!
//! The extracted tree only needs to be good enough to copy the binaries out,
//! it doesn't have to mirror the archive exactly:
//!
//! - Symlinks are only known from the central directory (unix mode), so they
//!   are first written as regular files holding the link target and turned
//!   into symlinks once the archive has been validated.
//! - Entries whose parent is such a not-yet-known entry are spooled to
//!   temporary files and written after the symlinks exist.
//!
//! Like the tar extractor, everything here runs on a blocking thread:
//! `async_zip` only has an async API, so it is driven with `block_on`, which
//! never touches the tokio runtime.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, create_dir_all, File},
    io::{self, BufRead, Seek, Write},
    path::{Path, PathBuf},
};

use async_zip::base::read::{
    cd::{CentralDirectoryReader, Entry},
    stream::ZipFileReader,
    ZipEntryReader,
};
use bytes::Bytes;
use futures_lite::{
    future::block_on,
    io::{AsyncBufRead, AsyncReadExt},
};
use futures_util::io::AllowStdIo;
use normalize_path::NormalizePath;
use tokio::sync::mpsc;
use tracing::warn;

use super::{DownloadError, ExtractedFiles};
use crate::utils::StreamReadable;

const COPY_BUF_SIZE: usize = 64 * 1024;

const S_IFMT: u32 = 0o170_000;
const S_IFLNK: u32 = 0o120_000;

fn create_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .expect("all full entry paths should have parent paths");
    create_dir_all(parent)
}

/// Strip the root so `/foo` lands inside the extraction dir, then normalize,
/// rejecting names that escape it (`..`) or carry a prefix (`C:`).
fn sanitized_name(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    path.strip_prefix("/").unwrap_or(path).try_normalize()
}

/// Where the streaming pass put a local entry.
enum Output {
    Skipped,
    Dir,
    /// Written as a regular file; may still turn out to be a symlink.
    File(PathBuf),
    /// Its parent is a [`Output::File`] that may turn out to be a symlink.
    Deferred(PathBuf, File),
}

/// What the streaming pass computed for one local file header.
struct LocalEntry {
    name: Vec<u8>,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    output: Output,
}

/// Extract the zip archive streamed through `rx` into `dir`.
pub(super) fn do_extract_zip(
    rx: mpsc::Receiver<Bytes>,
    dir: &Path,
) -> Result<ExtractedFiles, DownloadError> {
    extract_zip(StreamReadable::new(rx), dir)
}

fn extract_zip(reader: impl BufRead + Unpin, dir: &Path) -> Result<ExtractedFiles, DownloadError> {
    let mut created = Vec::new();
    let res = block_on(Extractor::new(dir, &mut created).run(reader));
    if res.is_err() {
        // The archive is invalid, don't leave anything it wrote behind.
        for path in created.iter().rev() {
            let _ = fs::remove_file(path);
        }
    }
    res
}

fn mismatch(name: &[u8], what: &str, expected: u64, actual: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "zip entry {}: {what} mismatch, expected {expected}, got {actual}",
            String::from_utf8_lossy(name)
        ),
    )
}

struct Extractor<'a> {
    dir: &'a Path,
    /// Every file and symlink created, for cleanup on error.
    created: &'a mut Vec<PathBuf>,
    buf: Vec<u8>,
}

impl<'a> Extractor<'a> {
    fn new(dir: &'a Path, created: &'a mut Vec<PathBuf>) -> Self {
        Self {
            dir,
            created,
            // async_zip reads into `&mut [u8]`, so initialize one buffer up
            // front and reuse it for every entry.
            buf: vec![0; COPY_BUF_SIZE],
        }
    }

    async fn run(mut self, reader: impl BufRead + Unpin) -> Result<ExtractedFiles, DownloadError> {
        let mut reader = AllowStdIo::new(reader);

        let (mut locals, cd_offset) = self.extract_local_entries(&mut reader).await?;

        let mut extracted_files = ExtractedFiles::new();
        let mut symlinks = Vec::new();
        let mut deferred = Vec::new();

        let mut num_entries = 0;
        let mut cd = CentralDirectoryReader::new(&mut reader, cd_offset);
        loop {
            match cd.next().await? {
                Entry::CentralDirectoryEntry(entry) => {
                    num_entries += 1;

                    let Some(local) = locals.remove(&entry.file_offset()) else {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "zip central directory entry {} has no local file header",
                                String::from_utf8_lossy(entry.filename().as_bytes())
                            ),
                        )
                        .into());
                    };

                    let name = &local.name;
                    if entry.filename().as_bytes() != name.as_slice() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "zip entry name mismatch, local file header {}, central directory {}",
                                String::from_utf8_lossy(name),
                                String::from_utf8_lossy(entry.filename().as_bytes())
                            ),
                        )
                        .into());
                    }
                    if entry.crc32() != local.crc32 {
                        return Err(mismatch(
                            name,
                            "crc32",
                            entry.crc32().into(),
                            local.crc32.into(),
                        )
                        .into());
                    }
                    if entry.uncompressed_size() != local.uncompressed_size {
                        return Err(mismatch(
                            name,
                            "size",
                            entry.uncompressed_size(),
                            local.uncompressed_size,
                        )
                        .into());
                    }
                    if entry.compressed_size() != local.compressed_size {
                        return Err(mismatch(
                            name,
                            "compressed size",
                            entry.compressed_size(),
                            local.compressed_size,
                        )
                        .into());
                    }

                    let is_symlink = cfg!(any(unix, target_os = "wasi"))
                        && entry
                            .unix_permissions()
                            .is_some_and(|mode| mode & S_IFMT == S_IFLNK);

                    match local.output {
                        Output::Skipped | Output::Dir => (),
                        Output::File(name) if is_symlink => symlinks.push(name),
                        Output::File(name) => extracted_files.add_file(&name),
                        Output::Deferred(name, spool) => deferred.push((name, spool, is_symlink)),
                    }
                }
                Entry::EndOfCentralDirectoryRecord {
                    record, extensible, ..
                } => {
                    if extensible {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "zip64 end of central directory record with extensible data",
                        )
                        .into());
                    }
                    if record.num_entries() != num_entries {
                        return Err(mismatch(
                            b"<archive>",
                            "number of entries",
                            record.num_entries(),
                            num_entries,
                        )
                        .into());
                    }
                    break;
                }
            }
        }

        if let Some(local) = locals.values().next() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "zip local file header {} has no central directory entry",
                    String::from_utf8_lossy(&local.name)
                ),
            )
            .into());
        }

        // The archive is valid, now materialize symlinks and whatever had to
        // wait for them.
        for name in symlinks {
            self.make_symlink(&name, &mut extracted_files)?;
        }
        for (name, mut spool, is_symlink) in deferred {
            let path = self.dir.join(&name);
            create_parent_dir(&path)?;
            let mut file = File::create_new(&path)?;
            self.created.push(path);
            spool.rewind()?;
            io::copy(&mut spool, &mut file)?;
            drop(file);

            if is_symlink {
                self.make_symlink(&name, &mut extracted_files)?;
            } else {
                extracted_files.add_file(&name);
            }
        }

        Ok(extracted_files)
    }

    /// Extract every entry from its local file header, verifying its CRC32 and
    /// sizes against the header or its data descriptor.
    ///
    /// Returns the entries keyed by offset, and the central directory offset.
    async fn extract_local_entries<R: AsyncBufRead + Unpin>(
        &mut self,
        reader: &mut R,
    ) -> Result<(HashMap<u64, LocalEntry>, u64), DownloadError> {
        let mut locals = HashMap::new();
        // Entries that may turn out to be symlinks, and entries deferred
        // because of them.
        let mut pending = HashSet::new();
        let mut offset = 0;

        let mut zip = ZipFileReader::new(reader);
        while let Some(mut entry) = zip.next_with_entry().await? {
            let header = entry.reader().entry();
            let file_offset = header.file_offset();
            let raw_name = header.filename().as_bytes().to_vec();
            let header_crc32 = header.crc32();
            let header_size = header.uncompressed_size();
            let header_compressed_size = header.compressed_size();
            let data_descriptor = header.data_descriptor();
            let name = header
                .filename()
                .as_str()
                .ok()
                .map(|name| (sanitized_name(name), name.ends_with('/')));

            let reader = entry.reader_mut();
            let (output, size) = match name {
                None => {
                    warn!("Skip zip entry with non-UTF-8 name, report to packager please");
                    (
                        Output::Skipped,
                        copy(reader, &mut io::sink(), &mut self.buf).await?,
                    )
                }
                Some((None, _)) => {
                    warn!(
                        "Skip zip entry {} for suspected zip slip",
                        String::from_utf8_lossy(&raw_name)
                    );
                    (
                        Output::Skipped,
                        copy(reader, &mut io::sink(), &mut self.buf).await?,
                    )
                }
                Some((Some(name), true)) => {
                    // Directories are created on demand anyway, so one that
                    // may live under a symlink can simply be dropped.
                    if !name.ancestors().any(|a| pending.contains(a)) {
                        create_dir_all(self.dir.join(name))?;
                    }
                    (
                        Output::Dir,
                        copy(reader, &mut io::sink(), &mut self.buf).await?,
                    )
                }
                Some((Some(name), false))
                    if name.ancestors().skip(1).any(|a| pending.contains(a)) =>
                {
                    let mut spool = tempfile::tempfile()?;
                    let size = copy(reader, &mut spool, &mut self.buf).await?;
                    pending.insert(name.clone());
                    (Output::Deferred(name, spool), size)
                }
                Some((Some(name), false)) => {
                    let path = self.dir.join(&name);
                    create_parent_dir(&path)?;
                    let mut file = File::create_new(&path)?;
                    self.created.push(path);
                    let size = copy(reader, &mut file, &mut self.buf).await?;
                    pending.insert(name.clone());
                    (Output::File(name), size)
                }
            };
            if matches!(output, Output::Dir) && size != 0 {
                return Err(mismatch(&raw_name, "directory size", 0, size).into());
            }

            let compressed_size = reader.bytes_read();
            let crc32 = reader.compute_hash();

            let (descriptor, next) = entry.skip().await?;
            zip = next;

            // With a data descriptor, the local header usually has zeros.
            let expected = match (data_descriptor, descriptor) {
                (false, None) => (header_crc32, header_size, header_compressed_size),
                (true, Some(descriptor)) => {
                    let lenient = |header: u64, actual: u64| header == 0 || header == actual;
                    if !lenient(header_crc32.into(), crc32.into())
                        || !lenient(header_size, size)
                        || !lenient(header_compressed_size, compressed_size)
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "zip entry {}: local file header disagrees with its data descriptor",
                                String::from_utf8_lossy(&raw_name)
                            ),
                        )
                        .into());
                    }
                    (
                        descriptor.crc,
                        descriptor.uncompressed_size,
                        descriptor.compressed_size,
                    )
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "zip entry {}: data descriptor flag mismatch",
                            String::from_utf8_lossy(&raw_name)
                        ),
                    )
                    .into())
                }
            };
            if expected.0 != crc32 {
                return Err(mismatch(&raw_name, "crc32", expected.0.into(), crc32.into()).into());
            }
            if expected.1 != size {
                return Err(mismatch(&raw_name, "size", expected.1, size).into());
            }
            if expected.2 != compressed_size {
                return Err(
                    mismatch(&raw_name, "compressed size", expected.2, compressed_size).into(),
                );
            }

            let local = LocalEntry {
                name: raw_name,
                crc32,
                compressed_size,
                uncompressed_size: size,
                output,
            };
            if locals.insert(file_offset, local).is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate zip local file header offset",
                )
                .into());
            }

            offset = zip.offset();
        }

        Ok((locals, offset))
    }

    /// Turn the regular file at `name`, holding a link target, into a symlink.
    #[cfg(any(unix, target_os = "wasi"))]
    fn make_symlink(
        &mut self,
        name: &Path,
        extracted_files: &mut ExtractedFiles,
    ) -> Result<(), DownloadError> {
        use std::ffi::OsStr;

        #[cfg(unix)]
        use std::os::unix::{ffi::OsStrExt, fs::symlink};

        #[cfg(target_os = "wasi")]
        use std::os::wasi::{ffi::OsStrExt, fs::symlink_path as symlink};

        const MAX_LINK_TARGET: u64 = 4096;

        let path = &self.dir.join(name);

        let src = if fs::symlink_metadata(path)?.len() <= MAX_LINK_TARGET {
            fs::read(path)?
        } else {
            Vec::new()
        };
        fs::remove_file(path)?;

        if src.is_empty() || src.contains(&0) {
            warn!(
                "Skip zip symlink dest={} with invalid target",
                path.display(),
            );
            return Ok(());
        }

        let src = Path::new(OsStr::from_bytes(&src));
        let Some(src) = &src.try_normalize() else {
            warn!(
                "Skip zip symlink {} => {} with target pointing outside, beware of possible malware",
                src.display(),
                path.display(),
            );
            return Ok(());
        };

        // src is relative to link_dir
        let link_dir = name.parent().unwrap_or(Path::new(""));
        if link_dir.join(src) == name {
            warn!("Skip symlink loop {}", path.display());
            return Ok(());
        }

        symlink(src, path)?;
        extracted_files.add_file(name);

        Ok(())
    }

    /// Symlinks are never detected here, they stay regular files holding
    /// the link target.
    #[cfg(not(any(unix, target_os = "wasi")))]
    fn make_symlink(&mut self, _: &Path, _: &mut ExtractedFiles) -> Result<(), DownloadError> {
        unreachable!("symlinks are only detected on unix and wasi")
    }
}

/// Copy an entry to `writer`, returning its uncompressed size.
async fn copy<R: AsyncBufRead + Unpin, E>(
    reader: &mut ZipEntryReader<'_, R, E>,
    writer: &mut impl Write,
    buf: &mut [u8],
) -> io::Result<u64> {
    let mut size = 0;
    loop {
        let n = reader.read(buf).await?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        size += n as u64;
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
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
        /// Leave the entry out of the central directory.
        hidden: bool,
    }

    impl TestEntry {
        fn file(name: &str, data: &[u8]) -> Self {
            Self {
                name: name.into(),
                mode: MODE_FILE,
                data: data.to_vec(),
                crc: None,
                local_name: None,
                hidden: false,
            }
        }

        fn dir(name: &str) -> Self {
            Self {
                name: format!("{}/", name.trim_end_matches('/')).into(),
                mode: MODE_DIR,
                data: Vec::new(),
                crc: None,
                local_name: None,
                hidden: false,
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
                hidden: false,
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

            if e.hidden {
                continue;
            }

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
        let count = u16::try_from(entries.iter().filter(|e| !e.hidden).count()).unwrap();
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
            let out = self.out();
            fs::create_dir_all(&out).unwrap();
            extract_zip(io::Cursor::new(zip.to_vec()), &out)
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
        // `..` that stays inside is normalized away.
        sb.extract(&[TestEntry::file("a/../b.txt", b"b")]).unwrap();

        sb.assert_root_contains_only(&["out"]);
        assert_eq!(fs::read(sb.out().join("b.txt")).unwrap(), b"b");
    }

    #[test]
    fn duplicate_file_entries_are_an_error() {
        let sb = Sandbox::new();
        let res = sb.extract(&[TestEntry::file("x", b"1"), TestEntry::file("x", b"2")]);

        assert!(res.is_err());
        // Whatever an invalid archive wrote is cleaned up.
        assert!(!sb.out().join("x").exists());
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
    fn local_entry_missing_from_central_directory_is_rejected() {
        // Streaming parsers see it, central-directory parsers don't.
        let sb = Sandbox::new();
        let mut hidden = TestEntry::file("hidden.txt", b"evil");
        hidden.hidden = true;
        let res = sb.extract(&[TestEntry::file("a.txt", b"a"), hidden]);

        assert!(res.is_err());
        assert!(fs::read_dir(sb.out()).unwrap().next().is_none());
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

            sb.extract(&[TestEntry::symlink("link", victim.as_os_str().as_bytes())])
                .unwrap();
            assert!(!exists_nofollow(&sb.out().join("link")));

            // Same name twice is rejected outright.
            let sb2 = Sandbox::new();
            let res = sb2.extract(&[
                TestEntry::symlink("link", victim.as_os_str().as_bytes()),
                TestEntry::file("link", b"PWNED"),
            ]);
            assert!(res.is_err());

            // Writing below it lands in a real directory inside.
            let sb3 = Sandbox::new();
            sb3.extract(&[
                TestEntry::symlink("dir", victim.parent().unwrap().as_os_str().as_bytes()),
                TestEntry::file("dir/victim", b"PWNED"),
            ])
            .unwrap();
            assert_eq!(fs::read(sb3.out().join("dir/victim")).unwrap(), b"PWNED");
            sb3.assert_root_contains_only(&["out"]);

            assert_eq!(fs::read(&victim).unwrap(), b"original");
            sb.assert_root_contains_only(&["out", "victim"]);
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
        fn directory_entry_below_symlink_is_dropped() {
            let sb = Sandbox::new();
            let files = sb
                .extract(&[
                    TestEntry::dir("real"),
                    TestEntry::symlink("d", "real"),
                    TestEntry::dir("d/sub"),
                    TestEntry::file("d/sub/x.txt", b"x"),
                ])
                .unwrap();

            assert_eq!(fs::read(sb.out().join("real/sub/x.txt")).unwrap(), b"x");
            assert!(files.has_file(Path::new("d/sub/x.txt")));
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
            assert!(!sb.out().join("target").exists());
            assert!(!exists_nofollow(&sb.out().join("l")));
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
