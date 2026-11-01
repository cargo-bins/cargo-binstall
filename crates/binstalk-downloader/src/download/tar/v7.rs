//! Upgrade legacy (pre-ustar, "UNIX v7") tar archives to ustar.
//!
//! `tar_codec` deliberately does not decode v7 archives, but they are still
//! produced in the wild, e.g. by `tar::Header::new_old` of the `tar` crate,
//! and archives published by cargo-quickinstall are v7 archives.
//!
//! v7 is a strict subset of ustar: it has no extension headers, so framing
//! only depends on the `size` of each header. This adapter rewrites every v7
//! header into an equivalent ustar header (set magic/version, recompute
//! checksum) and passes everything else through unchanged, leaving all
//! validation to `tar_codec`.
//!
//! The adapter is conservative: it is only enabled if the first header of the
//! archive is a v7 header. A ustar/pax/GNU header after v7 headers is an
//! error, and any other header it does not understand is forwarded unchanged,
//! in which case `tar_codec` rejects the archive since it has no magic.

use std::{
    io,
    pin::Pin,
    task::{ready, Context, Poll},
};

use tar_framing::{
    header::{
        CHECKSUM_RANGE as CHECKSUM, GID_RANGE as GID, IDENTITY_RANGE as MAGIC_AND_VERSION,
        MODE_RANGE as MODE, MTIME_RANGE as MTIME, NAME_RANGE as NAME, SIZE_RANGE as SIZE,
        TYPEFLAG_OFFSET as TYPEFLAG, UID_RANGE as UID, USTAR_IDENTITY,
    },
    Block, BLOCK_SIZE,
};
use tokio::io::{AsyncRead, ReadBuf};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum State {
    /// The first header has not been read yet.
    First,
    /// The archive is a v7 archive, `data_blocks` blocks of payload remain
    /// before the next header.
    V7 { data_blocks: u64 },
    /// Forward everything unchanged.
    Passthrough,
    /// A ustar/pax/GNU header follows v7 headers.
    Mixed,
}

pub(crate) struct V7ToUstar<R> {
    inner: R,
    block: Block,
    /// Number of bytes read into `block`.
    filled: usize,
    /// Number of bytes of a processed `block` already returned.
    ///
    /// `None` if `block` is not yet fully read and processed.
    emitted: Option<usize>,
    state: State,
}

impl<R> V7ToUstar<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self {
            inner,
            block: [0; BLOCK_SIZE],
            filled: 0,
            emitted: None,
            state: State::First,
        }
    }

    fn process_block(&mut self) {
        let block = &mut self.block;

        self.state = match self.state {
            State::Passthrough | State::Mixed => self.state,
            State::V7 { data_blocks } if data_blocks > 0 => State::V7 {
                data_blocks: data_blocks - 1,
            },
            // End of archive, the rest are paddings.
            _ if block.iter().all(|byte| *byte == 0) => State::Passthrough,
            // Not a v7 archive.
            State::First if block[MAGIC_AND_VERSION].iter().any(|byte| *byte != 0) => {
                State::Passthrough
            }
            // Reject mixing v7 with ustar/pax/GNU headers: forwarding it
            // unchanged would let tar_codec accept e.g. pax extensions after
            // the upgraded v7 headers, an archive that other implementations
            // may interpret differently.
            State::V7 { .. } if block[MAGIC_AND_VERSION].iter().any(|byte| *byte != 0) => {
                State::Mixed
            }
            State::First | State::V7 { .. } => match upgrade_header(block) {
                Some(size) => State::V7 {
                    data_blocks: size.div_ceil(BLOCK_SIZE as u64),
                },
                // Leave it to tar_codec to reject it.
                None => State::Passthrough,
            },
        };
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for V7ToUstar<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        loop {
            if this.state == State::Mixed {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "tar archive mixes v7 headers with ustar/pax/GNU headers",
                )));
            }

            if let Some(emitted) = this.emitted {
                if emitted < this.filled {
                    let n = buf.remaining().min(this.filled - emitted);
                    buf.put_slice(&this.block[emitted..emitted + n]);
                    this.emitted = Some(emitted + n);
                    return Poll::Ready(Ok(()));
                }

                this.emitted = None;
                this.filled = 0;
            }

            if this.state == State::Passthrough {
                return Pin::new(&mut this.inner).poll_read(cx, buf);
            }

            if buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }

            let mut read_buf = ReadBuf::new(&mut this.block[this.filled..]);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut read_buf))?;
            let n = read_buf.filled().len();
            this.filled += n;

            if n == 0 {
                // EOF: forward the incomplete block as is, tar_codec would
                // reject it.
                if this.filled == 0 {
                    return Poll::Ready(Ok(()));
                }
                this.state = State::Passthrough;
                this.emitted = Some(0);
            } else if this.filled == BLOCK_SIZE {
                this.process_block();
                this.emitted = Some(0);
            }
        }
    }
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

/// Write `value` as a zero-padded, NUL terminated octal number.
fn write_octal(field: &mut [u8], value: u64) -> Option<()> {
    let digits = field.len() - 1;
    let s = format!("{value:0digits$o}");
    if s.len() > digits {
        return None;
    }
    field[..digits].copy_from_slice(s.as_bytes());
    field[digits] = 0;
    Some(())
}

fn checksum(block: &Block, signed: bool) -> u64 {
    block
        .iter()
        .enumerate()
        .map(|(i, byte)| {
            let byte = if CHECKSUM.contains(&i) { b' ' } else { *byte };
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

/// Upgrade the v7 header to ustar in place.
///
/// Returns the size of the payload on success, or `None` if `block` is not
/// a valid v7 header, in which case `block` is left unmodified.
fn upgrade_header(block: &mut Block) -> Option<u64> {
    if block[MAGIC_AND_VERSION].iter().any(|byte| *byte != 0) {
        return None;
    }

    // Verify the checksum before modifying the header, so that a corrupted
    // header is never "fixed" by recomputing the checksum.
    let stored = parse_octal(&block[CHECKSUM]).ok()??;
    if stored != checksum(block, false) && stored != checksum(block, true) {
        return None;
    }

    let size = parse_octal(&block[SIZE]).ok()?.unwrap_or(0);

    let name = &block[NAME];
    let name = &name[..name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len())];

    let typeflag = match block[TYPEFLAG] {
        // v7 marks directories using a trailing slash.
        0 | b'0' if name.ends_with(b"/") => b'5',
        0 | b'0' => b'0',
        typeflag @ (b'1' | b'2' | b'5') => typeflag,
        // Not a v7 typeflag.
        _ => return None,
    };

    let mut upgraded = *block;

    // Canonicalize numeric fields since historic implementations pad them with
    // leading spaces, which ustar does not allow.
    for field in [MODE, UID, GID, SIZE, MTIME] {
        if let Some(value) = parse_octal(&upgraded[field.clone()]).ok()? {
            write_octal(&mut upgraded[field], value)?;
        }
    }

    upgraded[TYPEFLAG] = typeflag;
    upgraded[MAGIC_AND_VERSION].copy_from_slice(USTAR_IDENTITY);

    let checksum = checksum(&upgraded, false);
    upgraded[CHECKSUM].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());

    *block = upgraded;

    Some(size)
}

#[cfg(test)]
mod test {
    use super::*;

    use tokio::io::AsyncReadExt;

    fn v7_header(name: &str, typeflag: u8, size: u64) -> Block {
        let mut header = [0u8; BLOCK_SIZE];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[MODE].copy_from_slice(b"   755 \0");
        header[UID].copy_from_slice(b"0000765 ");
        header[GID].copy_from_slice(b"0000024 ");
        header[SIZE].copy_from_slice(format!("{size:011o} ").as_bytes());
        header[MTIME].copy_from_slice(b"15057527731 ");
        header[TYPEFLAG] = typeflag;
        let checksum = checksum(&header, false);
        header[CHECKSUM].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        header
    }

    async fn upgrade(input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        // Use a small buffer to exercise partial reads.
        let mut buf = [0; 100];
        let mut reader = V7ToUstar::new(input);
        loop {
            let n = reader.read(&mut buf).await.unwrap();
            if n == 0 {
                break output;
            }
            output.extend_from_slice(&buf[..n]);
        }
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
    async fn upgrades_v7_archive() {
        let mut input = Vec::new();
        input.extend_from_slice(&v7_header("dir/", 0, 0));
        input.extend_from_slice(&v7_header("dir/file", 0, 513));
        // Payload looks like a v7 header, it must not be rewritten.
        let payload_header = v7_header("payload", 0, 0);
        input.extend_from_slice(&payload_header);
        input.extend_from_slice(&[1; BLOCK_SIZE]);
        input.extend_from_slice(&v7_header("link", b'2', 0));
        input.extend_from_slice(&[0; BLOCK_SIZE * 2]);

        let output = upgrade(&input).await;
        assert_eq!(output.len(), input.len());

        let blocks: Vec<&[u8]> = output.chunks(BLOCK_SIZE).collect();
        for (block, typeflag) in [(blocks[0], b'5'), (blocks[1], b'0'), (blocks[4], b'2')] {
            assert_eq!(&block[MAGIC_AND_VERSION], b"ustar\x0000");
            assert_eq!(block[TYPEFLAG], typeflag);
            assert_eq!(&block[MODE], b"0000755\0");
            let block: &Block = block.try_into().unwrap();
            let stored = parse_octal(&block[CHECKSUM]).unwrap().unwrap();
            assert_eq!(stored, checksum(block, false));
        }
        assert_eq!(blocks[2], payload_header);
        assert_eq!(blocks[3], [1; BLOCK_SIZE]);
        assert_eq!(&output[BLOCK_SIZE * 5..], &[0; BLOCK_SIZE * 2]);
    }

    #[tokio::test]
    async fn leaves_invalid_header_untouched() {
        let mut header = v7_header("file", 0, 0);
        // Corrupt the checksum
        header[CHECKSUM.start] = b'7';
        let mut input = header.to_vec();
        input.extend_from_slice(&v7_header("file2", 0, 0));
        assert_eq!(upgrade(&input).await, input);

        // Unknown typeflag
        let input = v7_header("file", b'L', 0);
        assert_eq!(upgrade(&input).await, input);
    }

    #[tokio::test]
    async fn passthrough_ustar_and_truncated() {
        let mut header = v7_header("file", b'0', 0);
        header[MAGIC_AND_VERSION].copy_from_slice(b"ustar\x0000");
        let mut input = header.to_vec();
        // A v7 header after a ustar header is not rewritten.
        input.extend_from_slice(&v7_header("file2", 0, 0));
        assert_eq!(upgrade(&input).await, input);

        let input = &v7_header("file", 0, 0)[..300];
        assert_eq!(upgrade(input).await, input);
    }

    #[tokio::test]
    async fn rejects_ustar_after_v7() {
        let mut ustar = v7_header("file2", b'0', 0);
        ustar[MAGIC_AND_VERSION].copy_from_slice(b"ustar\x0000");

        let mut input = v7_header("file", 0, 0).to_vec();
        input.extend_from_slice(&ustar);

        let mut reader = V7ToUstar::new(&input[..]);
        let mut output = Vec::new();
        let err = reader.read_to_end(&mut output).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        // Only the upgraded v7 header is returned.
        assert_eq!(output.len(), BLOCK_SIZE);
        // The error is sticky.
        assert!(reader.read(&mut [0; 16]).await.is_err());
    }
}
