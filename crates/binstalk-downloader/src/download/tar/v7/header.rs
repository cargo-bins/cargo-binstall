//! Parsing of v7 tar headers.

use tar_codec::{Member, MemberMetadata};
use tar_framing::{
    header::{
        CHECKSUM_RANGE, GID_RANGE, IDENTITY_RANGE, LINK_NAME_RANGE, MODE_RANGE, MTIME_RANGE,
        NAME_RANGE, SIZE_RANGE, TYPEFLAG_OFFSET, UID_RANGE,
    },
    Block,
};

use super::{invalid, V7Error};

/// A parsed v7 header.
pub(super) struct Header {
    pub(super) member: Member<()>,
    pub(super) size: u64,
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

pub(super) fn checksum(block: &Block, signed: bool) -> u64 {
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

/// Parse and validate a v7 header located at byte `position`.
pub(super) fn parse_header(position: u64, block: &Block) -> Result<Header, V7Error> {
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
}
