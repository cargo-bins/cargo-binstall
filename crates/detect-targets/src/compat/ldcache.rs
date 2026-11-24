//! A reader for glibc's `/etc/ld.so.cache`, which maps sonames to the
//! libraries `ldconfig` found in the directories of `/etc/ld.so.conf`.
//!
//! Only the "new" format (`glibc-ld.so.cache1.1`) is read, whether on its
//! own (the default since glibc 2.32) or appended to the old format.

use std::{fs, io, path::PathBuf};

const OLD_MAGIC: &[u8] = b"ld.so-1.7.0";
const NEW_MAGIC: &[u8] = b"glibc-ld.so.cache1.1";

/// Size of `struct cache_file` (the old header).
const OLD_HEADER: usize = 16;
/// Size of `struct file_entry` (an old entry).
const OLD_ENTRY: usize = 12;
/// Size of `struct cache_file_new` (the new header).
const NEW_HEADER: usize = 48;
/// Size of `struct file_entry_new` (a new entry).
const NEW_ENTRY: usize = 24;

/// Real caches are well under a MiB.
const MAX_SIZE: u64 = 64 << 20;

pub(crate) const PATH: &str = "/etc/ld.so.cache";

/// Every `(soname, path)` pair in the cache, in cache order.
pub(crate) fn read(path: &str) -> io::Result<Vec<(String, PathBuf)>> {
    if fs::metadata(path)?.len() > MAX_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ld.so.cache too large",
        ));
    }
    parse(&fs::read(path)?)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unsupported ld.so.cache"))
}

fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    // The cache is written by the host's ldconfig in its own byte order.
    Some(u32::from_ne_bytes(buf.get(at..at + 4)?.try_into().ok()?))
}

fn str_at(buf: &[u8], at: usize) -> Option<&str> {
    let rest = buf.get(at..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    std::str::from_utf8(&rest[..end]).ok()
}

fn parse(buf: &[u8]) -> Option<Vec<(String, PathBuf)>> {
    let base = if buf.starts_with(NEW_MAGIC) {
        0
    } else if buf.starts_with(OLD_MAGIC) {
        let nlibs = usize::try_from(u32_at(buf, 12)?).ok()?;
        // The new format follows, aligned like `struct cache_file_new`.
        OLD_HEADER
            .checked_add(nlibs.checked_mul(OLD_ENTRY)?)?
            .next_multiple_of(8)
    } else {
        return None;
    };
    let new = buf.get(base..)?;
    if !new.starts_with(NEW_MAGIC) {
        return None;
    }

    let nlibs = usize::try_from(u32_at(new, 20)?).ok()?;
    // String offsets are relative to the start of the new header.
    let entries = new.get(NEW_HEADER..NEW_HEADER.checked_add(nlibs.checked_mul(NEW_ENTRY)?)?)?;
    Some(
        entries
            .as_chunks::<NEW_ENTRY>()
            .0
            .iter()
            .filter_map(|entry| {
                let key = str_at(new, u32_at(entry, 4)? as usize)?;
                let value = str_at(new, u32_at(entry, 8)? as usize)?;
                value
                    .starts_with('/')
                    .then(|| (key.to_owned(), PathBuf::from(value)))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_format(libs: &[(&str, &str)]) -> Vec<u8> {
        let mut strings = Vec::new();
        let strings_off = NEW_HEADER + libs.len() * NEW_ENTRY;
        let mut entries = Vec::new();
        for (key, value) in libs {
            let key_off = strings_off + strings.len();
            strings.extend_from_slice(key.as_bytes());
            strings.push(0);
            let value_off = strings_off + strings.len();
            strings.extend_from_slice(value.as_bytes());
            strings.push(0);

            entries.extend_from_slice(&0x303i32.to_ne_bytes()); // flags
            entries.extend_from_slice(&(key_off as u32).to_ne_bytes());
            entries.extend_from_slice(&(value_off as u32).to_ne_bytes());
            entries.extend_from_slice(&0u32.to_ne_bytes()); // osversion
            entries.extend_from_slice(&0u64.to_ne_bytes()); // hwcap
        }

        let mut buf = NEW_MAGIC.to_vec();
        buf.extend_from_slice(&(libs.len() as u32).to_ne_bytes());
        buf.extend_from_slice(&(strings.len() as u32).to_ne_bytes());
        buf.resize(NEW_HEADER, 0);
        buf.extend(entries);
        buf.extend(strings);
        buf
    }

    #[test]
    fn parses_new_format() {
        let buf = new_format(&[
            ("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
            ("libm.so.6", "/lib/x86_64-linux-gnu/libm.so.6"),
        ]);
        assert_eq!(
            parse(&buf).unwrap(),
            [
                ("libc.so.6".into(), "/lib/x86_64-linux-gnu/libc.so.6".into()),
                ("libm.so.6".into(), "/lib/x86_64-linux-gnu/libm.so.6".into()),
            ]
        );
    }

    #[test]
    fn parses_new_format_after_old() {
        let mut buf = OLD_MAGIC.to_vec();
        buf.push(0);
        buf.extend_from_slice(&3u32.to_ne_bytes());
        // Three old entries, then padding to the 8-byte boundary.
        buf.resize((OLD_HEADER + 3 * OLD_ENTRY).next_multiple_of(8), 0);
        buf.extend(new_format(&[("libz.so.1", "/usr/lib/libz.so.1")]));
        assert_eq!(
            parse(&buf).unwrap(),
            [("libz.so.1".into(), "/usr/lib/libz.so.1".into())]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"").is_none());
        assert!(parse(b"glibc-ld.so.cache1.1").is_none());
        let mut buf = new_format(&[("libc.so.6", "/lib/libc.so.6")]);
        // Claim more entries than there are.
        buf[20..24].copy_from_slice(&1000u32.to_ne_bytes());
        assert!(parse(&buf).is_none());
    }

    /// The host's own cache, if it has one, must parse and list libc.
    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn reads_host_cache() {
        if !std::path::Path::new(PATH).exists() {
            return;
        }
        let libs = read(PATH).unwrap();
        assert!(libs.iter().any(|(k, _)| k == "libc.so.6"), "{libs:?}");
    }
}
