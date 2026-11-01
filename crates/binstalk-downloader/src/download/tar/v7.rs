//! A strict decoder for legacy (pre-ustar, "UNIX v7") tar archives.
//!
//! `tar_codec` deliberately does not decode v7 archives, but they are still
//! produced in the wild, e.g. by `tar --format=v7` (used by
//! cargo-quickinstall) and by `tar::Header::new_old` of the `tar` crate.
//!
//! v7 has no extension headers, so framing only depends on the `size` of each
//! header, which makes it easy to decode without ambiguity as long as we are
//! strict about it:
//!
//! - Every header must be a v7 header: mixing in ustar/pax/GNU headers is
//!   rejected, as are non-zero bytes after the v7 header fields.
//! - The checksum must be valid.
//! - Only the v7 typeflags (regular file, hard link, symbolic link) and
//!   directories are supported; links and directories must have `size == 0`.
//! - Numeric fields must be octal; paths must be UTF-8.
//!
//! Members are exposed through [`Archive`], so extraction goes through the
//! same capability-relative [`Archive::extract_in`] as `tar_codec`.

use std::io;

use tar_codec::{Archive, Member, MemberMetadata, MemberPayload};
use tar_framing::{
    header::{
        CHECKSUM_RANGE, GID_RANGE, IDENTITY_RANGE, LINK_NAME_RANGE, MODE_RANGE, MTIME_RANGE,
        NAME_RANGE, SIZE_RANGE, TYPEFLAG_OFFSET, UID_RANGE,
    },
    Block, BLOCK_SIZE,
};
use thiserror::Error as ThisError;
use tokio::io::{AsyncRead, AsyncReadExt};

const BLOCK_SIZE_U64: u64 = BLOCK_SIZE as u64;

#[derive(Debug, ThisError)]
pub(crate) enum V7Error {
    #[error(transparent)]
    Io(io::Error),

    #[error("at byte {position}: invalid v7 tar archive: {reason}")]
    Invalid { position: u64, reason: &'static str },
}

fn invalid(position: u64, reason: &'static str) -> V7Error {
    V7Error::Invalid { position, reason }
}

/// Return true if `block` looks like a v7 header, i.e. it has no ustar/GNU
/// magic and is not the end-of-archive marker.
pub(crate) fn is_v7_header(block: &Block) -> bool {
    block[IDENTITY_RANGE].iter().all(|byte| *byte == 0) && block.iter().any(|byte| *byte != 0)
}

pub(crate) struct V7Archive<R> {
    reader: R,
    /// The first header, already read to detect the format.
    first: Option<Box<Block>>,
    /// Position of the next byte to read.
    position: u64,
    /// Remaining payload bytes of the current member.
    remaining: u64,
    /// Padding bytes following the payload of the current member.
    padding: u64,
    /// Set once the end of archive is reached or an error occurred.
    fused: bool,
}

impl<R: AsyncRead + Unpin> V7Archive<R> {
    /// `first` is the first block of the archive, which must satisfy
    /// [`is_v7_header`].
    pub(crate) fn new(reader: R, first: Block) -> Self {
        Self {
            reader,
            first: Some(Box::new(first)),
            position: BLOCK_SIZE_U64,
            remaining: 0,
            padding: 0,
            fused: false,
        }
    }

    async fn read_block(&mut self) -> Result<Option<Block>, V7Error> {
        let mut block = [0; BLOCK_SIZE];
        let mut filled = 0;

        while filled < BLOCK_SIZE {
            let n = self
                .reader
                .read(&mut block[filled..])
                .await
                .map_err(V7Error::Io)?;
            if n == 0 {
                return if filled == 0 {
                    Ok(None)
                } else {
                    Err(invalid(self.position, "incomplete block"))
                };
            }
            filled += n;
        }

        self.position += BLOCK_SIZE_U64;
        Ok(Some(block))
    }

    /// Discard the rest of the current member.
    async fn drain(&mut self) -> Result<(), V7Error> {
        let mut buf = [0; 8192];

        while self.remaining + self.padding > 0 {
            let to_skip = self.remaining + self.padding;
            let len = buf
                .len()
                .min(usize::try_from(to_skip).unwrap_or(usize::MAX));
            let n = self
                .reader
                .read(&mut buf[..len])
                .await
                .map_err(V7Error::Io)?;
            if n == 0 {
                return Err(invalid(self.position, "truncated member payload"));
            }
            let n = n as u64;
            let from_payload = n.min(self.remaining);
            self.remaining -= from_payload;
            self.padding -= n - from_payload;
            self.position += n;
        }

        Ok(())
    }

    async fn next_header(&mut self) -> Result<Option<Member<()>>, V7Error> {
        self.drain().await?;

        let block = match self.first.take() {
            Some(block) => *block,
            None => self
                .read_block()
                .await?
                .ok_or_else(|| invalid(self.position, "missing end-of-archive marker"))?,
        };
        // The header starts one block before the current position.
        let position = self.position - BLOCK_SIZE_U64;

        if block.iter().all(|byte| *byte == 0) {
            // End-of-archive marker is two zero blocks; anything after it
            // (e.g. padding to the record size) is ignored.
            return match self.read_block().await? {
                Some(block) if block.iter().all(|byte| *byte == 0) => Ok(None),
                _ => Err(invalid(position, "incomplete end-of-archive marker")),
            };
        }

        let header = parse_header(position, &block)?;
        self.remaining = header.size;
        self.padding = header.size.next_multiple_of(BLOCK_SIZE_U64) - header.size;

        Ok(Some(header.member))
    }
}

impl<R: AsyncRead + Unpin> Archive for V7Archive<R> {
    type Error = V7Error;
    type Payload<'a>
        = V7Payload<'a, R>
    where
        Self: 'a;

    async fn next_member<'a>(
        &'a mut self,
    ) -> Result<Option<Member<Self::Payload<'a>>>, Self::Error> {
        if self.fused {
            return Ok(None);
        }

        let member = match self.next_header().await {
            Ok(Some(member)) => member,
            res => {
                self.fused = true;
                return res.map(|_| None);
            }
        };

        let payload = V7Payload { archive: self };
        Ok(Some(match member {
            Member::File {
                metadata,
                size,
                executable,
                ..
            } => Member::File {
                metadata,
                size,
                executable,
                payload,
            },
            Member::HardLink {
                metadata,
                target,
                size,
                ..
            } => Member::HardLink {
                metadata,
                target,
                size,
                payload,
            },
            Member::Directory { metadata } => Member::Directory { metadata },
            Member::SymbolicLink { metadata, target } => Member::SymbolicLink { metadata, target },
            Member::Special { metadata, kind } => Member::Special { metadata, kind },
        }))
    }
}

pub(crate) struct V7Payload<'a, R> {
    archive: &'a mut V7Archive<R>,
}

impl<R: AsyncRead + Unpin> MemberPayload for V7Payload<'_, R> {
    type Error = V7Error;

    async fn next_chunk(
        &mut self,
        buffer: &mut Vec<u8>,
        target_len: usize,
    ) -> Result<bool, Self::Error> {
        let archive = &mut *self.archive;

        if archive.remaining == 0 {
            return Ok(false);
        }

        let len = usize::try_from(archive.remaining)
            .unwrap_or(usize::MAX)
            .min(target_len.max(1));
        buffer.clear();
        buffer.resize(len, 0);

        let n = match archive.reader.read(buffer).await {
            Ok(0) => Err(invalid(archive.position, "truncated member payload")),
            Ok(n) => Ok(n),
            Err(err) => Err(V7Error::Io(err)),
        }
        .inspect_err(|_| archive.fused = true)?;

        buffer.truncate(n);
        archive.remaining -= n as u64;
        archive.position += n as u64;

        Ok(true)
    }

    async fn skip(self) -> Result<(), Self::Error> {
        let archive = self.archive;
        archive.drain().await.inspect_err(|_| archive.fused = true)
    }
}

struct Header {
    member: Member<()>,
    size: u64,
}

/// Parse an octal number, allowing leading spaces as historic tar
/// implementations do.
///
/// Returns `Ok(None)` if the field is empty.
fn parse_octal(field: &[u8]) -> Result<Option<u64>, ()> {
    let mut value: Option<u64> = None;
    let mut terminated = false;

    for byte in field {
        match *byte {
            b' ' if value.is_none() && !terminated => (),
            b'0'..=b'7' if !terminated => {
                value = Some(
                    value
                        .unwrap_or(0)
                        .checked_mul(8)
                        .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                        .ok_or(())?,
                );
            }
            0 | b' ' => terminated = true,
            _ => return Err(()),
        }
    }

    if value.is_some() && !terminated {
        // The field must be terminated.
        return Err(());
    }

    Ok(value)
}

fn checksum(block: &Block, signed: bool) -> u64 {
    block
        .iter()
        .enumerate()
        .map(|(i, byte)| {
            let byte = if CHECKSUM_RANGE.contains(&i) {
                b' '
            } else {
                *byte
            };
            if signed {
                // Historic implementations computed the checksum using signed
                // char.
                i64::from(byte as i8) as u64
            } else {
                u64::from(byte)
            }
        })
        .fold(0, u64::wrapping_add)
}

fn str_field(position: u64, field: &[u8], reason: &'static str) -> Result<String, V7Error> {
    let field = &field[..field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len())];
    String::from_utf8(field.to_vec()).map_err(|_| invalid(position, reason))
}

fn parse_header(position: u64, block: &Block) -> Result<Header, V7Error> {
    if block[IDENTITY_RANGE].iter().any(|byte| *byte != 0) {
        return Err(invalid(
            position,
            "v7 archive must not contain ustar/pax/GNU headers",
        ));
    }
    if block[IDENTITY_RANGE.start..].iter().any(|byte| *byte != 0) {
        return Err(invalid(
            position,
            "non-zero bytes after the v7 header fields",
        ));
    }

    let stored = parse_octal(&block[CHECKSUM_RANGE])
        .ok()
        .flatten()
        .ok_or_else(|| invalid(position, "invalid checksum field"))?;
    if stored != checksum(block, false) && stored != checksum(block, true) {
        return Err(invalid(position, "checksum mismatch"));
    }

    let octal = |range: std::ops::Range<usize>, reason| {
        parse_octal(&block[range]).map_err(|_| invalid(position, reason))
    };
    let mode = octal(MODE_RANGE, "invalid mode field")?;
    octal(UID_RANGE, "invalid uid field")?;
    octal(GID_RANGE, "invalid gid field")?;
    octal(MTIME_RANGE, "invalid mtime field")?;
    let size = octal(SIZE_RANGE, "invalid size field")?
        .ok_or_else(|| invalid(position, "missing size field"))?;

    let path = str_field(position, &block[NAME_RANGE], "path is not valid UTF-8")?;
    if path.is_empty() {
        return Err(invalid(position, "empty path"));
    }
    let target = str_field(
        position,
        &block[LINK_NAME_RANGE],
        "link target is not valid UTF-8",
    )?;

    let metadata = MemberMetadata { path, position };

    let member = match block[TYPEFLAG_OFFSET] {
        // v7 marks directories using a trailing slash.
        0 | b'0' if metadata.path.ends_with('/') => Member::Directory { metadata },
        0 | b'0' => Member::File {
            metadata,
            size,
            executable: mode.unwrap_or(0) & 0o111 != 0,
            payload: (),
        },
        b'1' => Member::HardLink {
            metadata,
            target,
            size,
            payload: (),
        },
        b'2' => Member::SymbolicLink { metadata, target },
        // Not part of v7, but written by e.g. GNU tar for directories.
        b'5' => Member::Directory { metadata },
        _ => return Err(invalid(position, "unsupported typeflag")),
    };

    if !matches!(member, Member::File { .. }) && size != 0 {
        return Err(invalid(position, "non-file member with non-zero size"));
    }

    Ok(Header { member, size })
}

#[cfg(test)]
mod test {
    use super::*;

    fn v7_header(name: &str, typeflag: u8, size: u64) -> Block {
        let mut header = [0u8; BLOCK_SIZE];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[MODE_RANGE].copy_from_slice(b"   755 \0");
        header[UID_RANGE].copy_from_slice(b"0000765 ");
        header[GID_RANGE].copy_from_slice(b"0000024 ");
        header[SIZE_RANGE].copy_from_slice(format!("{size:011o} ").as_bytes());
        header[MTIME_RANGE].copy_from_slice(b"15057527731 ");
        header[TYPEFLAG_OFFSET] = typeflag;
        set_checksum(&mut header);
        header
    }

    fn set_checksum(header: &mut Block) {
        let checksum = checksum(header, false);
        header[CHECKSUM_RANGE].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
    }

    fn archive(blocks: &[Block]) -> Vec<u8> {
        let mut data: Vec<u8> = blocks.iter().flatten().copied().collect();
        data.extend_from_slice(&[0; BLOCK_SIZE * 2]);
        data
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Kind {
        File(Vec<u8>, bool),
        Dir,
        Symlink(String),
        HardLink(String),
    }

    async fn decode(data: &[u8]) -> Result<Vec<(String, Kind)>, V7Error> {
        let first: Block = data[..BLOCK_SIZE].try_into().unwrap();
        assert!(is_v7_header(&first));
        let mut members = V7Archive::new(&data[BLOCK_SIZE..], first).members();
        let mut out = Vec::new();
        while let Some(member) = members.next().await? {
            let path = member.metadata().path.clone();
            let kind = match member {
                Member::File {
                    mut payload,
                    executable,
                    ..
                } => {
                    let mut content = Vec::new();
                    let mut chunk = Vec::new();
                    // Small chunk size to exercise partial reads.
                    while payload.next_chunk(&mut chunk, 100).await? {
                        content.extend_from_slice(&chunk);
                    }
                    Kind::File(content, executable)
                }
                Member::Directory { .. } => Kind::Dir,
                Member::SymbolicLink { target, .. } => Kind::Symlink(target),
                Member::HardLink { target, .. } => Kind::HardLink(target),
                Member::Special { .. } => unreachable!(),
            };
            out.push((path, kind));
        }
        Ok(out)
    }

    fn file(name: &str, content: &[u8]) -> Vec<Block> {
        let mut blocks = vec![v7_header(name, 0, content.len() as u64)];
        for chunk in content.chunks(BLOCK_SIZE) {
            let mut block = [0; BLOCK_SIZE];
            block[..chunk.len()].copy_from_slice(chunk);
            blocks.push(block);
        }
        blocks
    }

    #[test]
    fn test_parse_octal() {
        assert_eq!(parse_octal(b"0000755 \0"), Ok(Some(0o755)));
        assert_eq!(parse_octal(b"   755 \0"), Ok(Some(0o755)));
        assert_eq!(parse_octal(b"\0\0\0\0"), Ok(None));
        assert_eq!(parse_octal(b"        "), Ok(None));
        assert_eq!(parse_octal(b"0000758\0"), Err(()));
        assert_eq!(parse_octal(b"00007550"), Err(()));
        assert_eq!(parse_octal(b"07 5\0"), Err(()));
    }

    #[tokio::test]
    async fn decodes_v7_archive() {
        // The payload looks like a v7 header, it must not be parsed as one.
        let mut big = v7_header("payload", 0, 0).to_vec();
        big.extend_from_slice(&[1; 600]);

        let mut blocks = vec![v7_header("dir/", 0, 0)];
        blocks.extend(file("dir/big", &big));
        blocks.extend(file("empty", b""));
        blocks.push(v7_header("link", b'2', 0));
        let mut hardlink = v7_header("hardlink", b'1', 0);
        hardlink[LINK_NAME_RANGE.start..LINK_NAME_RANGE.start + 5].copy_from_slice(b"empty");
        set_checksum(&mut hardlink);
        blocks.push(hardlink);
        blocks.push(v7_header("dir2", b'5', 0));
        let mut not_executable = v7_header("data", 0, 0);
        not_executable[MODE_RANGE].copy_from_slice(b"0000644\0");
        set_checksum(&mut not_executable);
        blocks.push(not_executable);

        assert_eq!(
            decode(&archive(&blocks)).await.unwrap(),
            [
                ("dir/".into(), Kind::Dir),
                ("dir/big".into(), Kind::File(big, true)),
                ("empty".into(), Kind::File(vec![], true)),
                ("link".into(), Kind::Symlink(String::new())),
                ("hardlink".into(), Kind::HardLink("empty".into())),
                ("dir2".into(), Kind::Dir),
                ("data".into(), Kind::File(vec![], false)),
            ]
        );
    }

    #[tokio::test]
    async fn skips_unread_payload_and_accepts_signed_checksum() {
        let mut blocks = file("a", &[0xff; 700]);
        // Header with a high byte, checksummed using signed char.
        let mut header = v7_header("\u{e9}", 0, 1);
        let checksum = checksum(&header, true);
        header[CHECKSUM_RANGE].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        blocks.push(header);
        blocks.push([b'x'; BLOCK_SIZE]);

        let data = archive(&blocks);
        let first: Block = data[..BLOCK_SIZE].try_into().unwrap();
        let mut members = V7Archive::new(&data[BLOCK_SIZE..], first).members();
        // Do not read the payload of `a`.
        assert_eq!(members.next().await.unwrap().unwrap().metadata().path, "a");
        let member = members.next().await.unwrap().unwrap();
        assert_eq!(member.metadata().path, "\u{e9}");
        assert_eq!(member.metadata().position, 512 * 3);
        drop(member);
        assert!(members.next().await.unwrap().is_none());
        // Fused
        assert!(members.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_invalid_archives() {
        let mut ustar = v7_header("b", b'0', 0);
        ustar[IDENTITY_RANGE].copy_from_slice(b"ustar\x0000");
        set_checksum(&mut ustar);

        let mut bad_checksum = v7_header("b", 0, 0);
        bad_checksum[CHECKSUM_RANGE.start] = b'7';

        let mut trailing = v7_header("b", 0, 0);
        trailing[400] = 1;
        set_checksum(&mut trailing);

        let mut bad_mode = v7_header("b", 0, 0);
        bad_mode[MODE_RANGE].copy_from_slice(b"0000x55\0");
        set_checksum(&mut bad_mode);

        let mut non_utf8 = v7_header("b", 0, 0);
        non_utf8[0] = 0xff;
        set_checksum(&mut non_utf8);

        for (name, second, reason) in [
            ("mixed ustar", ustar, "ustar/pax/GNU"),
            ("bad checksum", bad_checksum, "checksum mismatch"),
            ("trailing bytes", trailing, "non-zero bytes"),
            ("bad mode", bad_mode, "invalid mode"),
            ("non-utf8", non_utf8, "UTF-8"),
            (
                "pax typeflag",
                v7_header("b", b'x', 0),
                "unsupported typeflag",
            ),
            (
                "gnu typeflag",
                v7_header("b", b'L', 0),
                "unsupported typeflag",
            ),
            ("dir with size", v7_header("b/", 0, 1), "non-zero size"),
            (
                "symlink with size",
                v7_header("b", b'2', 1),
                "non-zero size",
            ),
        ] {
            let mut blocks = file("a", b"a");
            blocks.push(second);
            let err = decode(&archive(&blocks)).await.unwrap_err();
            assert!(err.to_string().contains(reason), "{name}: {err}");
        }

        // Truncated payload
        let data: Vec<u8> = file("a", &[1; 1000]).iter().flatten().copied().collect();
        let err = decode(&data[..800]).await.unwrap_err();
        assert!(err.to_string().contains("truncated"), "{err}");

        // Missing end-of-archive marker
        let data: Vec<u8> = file("a", b"a").iter().flatten().copied().collect();
        let err = decode(&data).await.unwrap_err();
        assert!(err.to_string().contains("end-of-archive"), "{err}");

        // Incomplete end-of-archive marker
        let mut data = data;
        data.extend_from_slice(&[0; BLOCK_SIZE]);
        let err = decode(&data).await.unwrap_err();
        assert!(err.to_string().contains("end-of-archive"), "{err}");
    }
}
