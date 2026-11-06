//! Parsing of v7 tar headers.

use std::mem::offset_of;

use tar_codec::{Member, MemberMetadata};
use tar_framing::{Block, BLOCK_SIZE};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

use super::{invalid, V7Error};

/// A NUL or space terminated octal number field.
///
/// Leading spaces are allowed, as historic tar implementations used them.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(transparent)]
pub(super) struct Octal<const N: usize>(pub(super) [u8; N]);

impl<const N: usize> Octal<N> {
    /// Returns `Ok(None)` if the field is empty.
    fn get(&self) -> Result<Option<u64>, ()> {
        parse_octal(&self.0)
    }
}

/// A NUL terminated (unless it fills the whole field) string field.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(transparent)]
pub(super) struct NulStr<const N: usize>(pub(super) [u8; N]);

impl<const N: usize> NulStr<N> {
    fn to_string(&self) -> Result<String, ()> {
        let len = self.0.iter().position(|byte| *byte == 0).unwrap_or(N);
        String::from_utf8(self.0[..len].to_vec()).map_err(|_| ())
    }
}

/// Layout of a v7 header.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub(super) struct V7Header {
    pub(super) name: NulStr<100>,
    pub(super) mode: Octal<8>,
    pub(super) uid: Octal<8>,
    pub(super) gid: Octal<8>,
    pub(super) size: Octal<12>,
    pub(super) mtime: Octal<12>,
    pub(super) checksum: Octal<8>,
    pub(super) typeflag: u8,
    pub(super) link_name: NulStr<100>,
    /// Where ustar/GNU put their magic and extra fields, must be zero for v7.
    pub(super) rest: [u8; 255],
}

const _: () = assert!(size_of::<V7Header>() == BLOCK_SIZE);

impl V7Header {
    pub(super) fn from_block(block: &Block) -> &Self {
        zerocopy::transmute_ref!(block)
    }

    /// The ustar/GNU magic and version, which are zero for v7.
    pub(super) fn identity(&self) -> &[u8] {
        &self.rest[..8]
    }

    #[cfg(test)]
    pub(super) fn from_block_mut(block: &mut Block) -> &mut Self {
        zerocopy::transmute_mut!(block)
    }

    /// Compute the checksum of the header, using either unsigned or (as some
    /// historic implementations did) signed bytes.
    pub(super) fn compute_checksum(&self, signed: bool) -> u64 {
        const CHECKSUM: usize = offset_of!(V7Header, checksum);

        self.as_bytes()
            .iter()
            .enumerate()
            .map(|(i, byte)| {
                // The checksum field itself is counted as spaces.
                let byte = if (CHECKSUM..CHECKSUM + 8).contains(&i) {
                    b' '
                } else {
                    *byte
                };
                if signed {
                    i64::from(byte as i8) as u64
                } else {
                    u64::from(byte)
                }
            })
            .fold(0, u64::wrapping_add)
    }
}

/// A parsed v7 header.
pub(super) struct Header {
    pub(super) member: Member<()>,
    pub(super) size: u64,
}

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

/// Parse and validate a v7 header located at byte `position`.
pub(super) fn parse_header(position: u64, block: &Block) -> Result<Header, V7Error> {
    let header = V7Header::from_block(block);

    if header.identity().iter().any(|byte| *byte != 0) {
        return Err(invalid(
            position,
            "v7 archive must not contain ustar/pax/GNU headers",
        ));
    }
    if header.rest.iter().any(|byte| *byte != 0) {
        return Err(invalid(
            position,
            "non-zero bytes after the v7 header fields",
        ));
    }

    let stored = header
        .checksum
        .get()
        .ok()
        .flatten()
        .ok_or_else(|| invalid(position, "invalid checksum field"))?;
    if stored != header.compute_checksum(false) && stored != header.compute_checksum(true) {
        return Err(invalid(position, "checksum mismatch"));
    }

    let octal =
        |field: Result<Option<u64>, ()>, reason| field.map_err(|_| invalid(position, reason));
    let mode = octal(header.mode.get(), "invalid mode field")?;
    octal(header.uid.get(), "invalid uid field")?;
    octal(header.gid.get(), "invalid gid field")?;
    octal(header.mtime.get(), "invalid mtime field")?;
    let size = octal(header.size.get(), "invalid size field")?
        .ok_or_else(|| invalid(position, "missing size field"))?;

    let path = header
        .name
        .to_string()
        .map_err(|_| invalid(position, "path is not valid UTF-8"))?;
    if path.is_empty() {
        return Err(invalid(position, "empty path"));
    }
    let target = header
        .link_name
        .to_string()
        .map_err(|_| invalid(position, "link target is not valid UTF-8"))?;

    let metadata = MemberMetadata { path, position };

    let member = match header.typeflag {
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

    #[test]
    fn layout_matches_tar_framing() {
        use tar_framing::header::{
            CHECKSUM_RANGE, GID_RANGE, IDENTITY_RANGE, LINK_NAME_RANGE, MODE_RANGE, MTIME_RANGE,
            NAME_RANGE, SIZE_RANGE, TYPEFLAG_OFFSET, UID_RANGE,
        };

        let range = |offset: usize, len: usize| offset..offset + len;
        assert_eq!(range(offset_of!(V7Header, name), 100), NAME_RANGE);
        assert_eq!(range(offset_of!(V7Header, mode), 8), MODE_RANGE);
        assert_eq!(range(offset_of!(V7Header, uid), 8), UID_RANGE);
        assert_eq!(range(offset_of!(V7Header, gid), 8), GID_RANGE);
        assert_eq!(range(offset_of!(V7Header, size), 12), SIZE_RANGE);
        assert_eq!(range(offset_of!(V7Header, mtime), 12), MTIME_RANGE);
        assert_eq!(range(offset_of!(V7Header, checksum), 8), CHECKSUM_RANGE);
        assert_eq!(offset_of!(V7Header, typeflag), TYPEFLAG_OFFSET);
        assert_eq!(range(offset_of!(V7Header, link_name), 100), LINK_NAME_RANGE);
        assert_eq!(offset_of!(V7Header, rest), IDENTITY_RANGE.start);
        assert_eq!(IDENTITY_RANGE.len(), 8);
    }
}
