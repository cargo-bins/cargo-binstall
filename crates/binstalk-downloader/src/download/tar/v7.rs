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
//! - The archive must end with a two-block end-of-archive marker.
//!
//! The decoding is done by the sans-io [`V7Decoder`]. [`V7Archive`] drives it
//! from an [`AsyncRead`] and exposes the members through [`Archive`], so
//! extraction goes through the same capability-relative
//! [`Archive::extract_in`] as `tar_codec`.

use std::io;

use tar_codec::{Archive, Member, MemberPayload};
use tar_framing::{header::IDENTITY_RANGE, Block, BLOCK_SIZE};
use thiserror::Error as ThisError;
use tokio::io::{AsyncRead, AsyncReadExt};

use super::map_payload;

mod header;
use header::{parse_header, Header};

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

fn is_zero_block(block: &[u8]) -> bool {
    block.iter().all(|byte| *byte == 0)
}

/// Sans-io v7 decoder.
///
/// [`V7Decoder::decode`] consumes a prefix of its input and returns what it
/// found. The decoder never performs IO and keeps no reference to the input,
/// so the caller is free to buffer and read however it wants.
#[derive(Debug, Default)]
pub(crate) struct V7Decoder {
    state: State,
    /// Position in the archive of the next byte to decode.
    position: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    /// Expecting a member header or the end-of-archive marker.
    #[default]
    Header,
    /// Inside the payload of the current member, which is followed by
    /// `padding` bytes.
    Payload { remaining: u64, padding: u64 },
    /// Inside the padding following a payload.
    Padding { remaining: u64 },
    /// The first zero block of the end-of-archive marker was decoded.
    SecondZero,
    /// The end-of-archive marker was decoded; anything after it (e.g.
    /// padding to the record size) is ignored.
    Done,
    /// An error was returned.
    Failed,
}

#[derive(Debug)]
pub(crate) enum Event {
    /// More input is needed, or [`V7Decoder::eof`] if there is none.
    NeedData,
    /// The header of a member, which is followed by its payload as
    /// [`Event::Payload`] if it has a non-zero `size`.
    Member(Member<()>),
    /// The first `len` bytes of the input are (part of) the payload of the
    /// current member.
    Payload { len: usize },
    /// The archive ended, or an error was returned before.
    End,
}

impl V7Decoder {
    /// Decode a prefix of `input`.
    ///
    /// Returns the number of bytes consumed along with the event, which must
    /// be dropped from the input before calling this again.
    pub(crate) fn decode(&mut self, input: &[u8]) -> Result<(usize, Event), V7Error> {
        let mut consumed = 0;
        let res = self.decode_inner(input, &mut consumed);

        self.position += consumed as u64;
        if res.is_err() {
            self.state = State::Failed;
        }

        res.map(|event| (consumed, event))
    }

    fn decode_inner(&mut self, input: &[u8], consumed: &mut usize) -> Result<Event, V7Error> {
        loop {
            let input = &input[*consumed..];
            let position = self.position + *consumed as u64;

            match self.state {
                State::Done | State::Failed => return Ok(Event::End),

                State::Payload { remaining, padding } => {
                    if input.is_empty() {
                        return Ok(Event::NeedData);
                    }
                    let len = input
                        .len()
                        .min(usize::try_from(remaining).unwrap_or(usize::MAX));
                    let remaining = remaining - len as u64;
                    *consumed += len;
                    self.state = match (remaining, padding) {
                        (0, 0) => State::Header,
                        (0, padding) => State::Padding { remaining: padding },
                        (remaining, padding) => State::Payload { remaining, padding },
                    };
                    return Ok(Event::Payload { len });
                }

                State::Padding { remaining } => {
                    if input.is_empty() {
                        return Ok(Event::NeedData);
                    }
                    let len = input
                        .len()
                        .min(usize::try_from(remaining).unwrap_or(usize::MAX));
                    let remaining = remaining - len as u64;
                    *consumed += len;
                    self.state = if remaining == 0 {
                        State::Header
                    } else {
                        State::Padding { remaining }
                    };
                }

                State::Header | State::SecondZero => {
                    let Some(block) = input.first_chunk::<BLOCK_SIZE>() else {
                        return Ok(Event::NeedData);
                    };

                    if self.state == State::SecondZero {
                        if !is_zero_block(block) {
                            return Err(invalid(
                                position - BLOCK_SIZE_U64,
                                "incomplete end-of-archive marker",
                            ));
                        }
                        *consumed += BLOCK_SIZE;
                        self.state = State::Done;
                        return Ok(Event::End);
                    }

                    if is_zero_block(block) {
                        *consumed += BLOCK_SIZE;
                        self.state = State::SecondZero;
                        continue;
                    }

                    let Header { member, size } = parse_header(position, block)?;
                    *consumed += BLOCK_SIZE;
                    let padding = size.next_multiple_of(BLOCK_SIZE_U64) - size;
                    self.state = if size == 0 {
                        State::Header
                    } else {
                        State::Payload {
                            remaining: size,
                            padding,
                        }
                    };
                    return Ok(Event::Member(member));
                }
            }
        }
    }

    /// Called when there is no more input after [`Event::NeedData`].
    pub(crate) fn eof(&mut self) -> V7Error {
        let (position, reason) = match self.state {
            State::Header => (self.position, "missing end-of-archive marker"),
            State::Payload { .. } | State::Padding { .. } => {
                (self.position, "truncated member payload")
            }
            // Point at the first zero block, as when the second block is not
            // zero.
            State::SecondZero => (
                self.position - BLOCK_SIZE_U64,
                "incomplete end-of-archive marker",
            ),
            State::Done | State::Failed => (self.position, "unexpected end of input"),
        };
        self.state = State::Failed;
        invalid(position, reason)
    }

    /// Return true if inside the payload of the current member.
    pub(crate) fn in_payload(&self) -> bool {
        matches!(self.state, State::Payload { .. })
    }
}

/// How much to read from the underlying reader at once.
const READ_SIZE: usize = 64 * 1024;

/// Drives [`V7Decoder`] from an [`AsyncRead`].
///
/// All methods are cancel safe: input is buffered in `self` and only dropped
/// once the decoder consumed it.
pub(crate) struct V7Archive<R> {
    reader: R,
    decoder: V7Decoder,
    /// Read but not yet consumed input is `buffer[start..]`.
    buffer: Vec<u8>,
    start: usize,
}

impl<R: AsyncRead + Unpin> V7Archive<R> {
    /// `first` is the first block of the archive, which must satisfy
    /// [`is_v7_header`].
    pub(crate) fn new(reader: R, first: Block) -> Self {
        Self {
            reader,
            decoder: V7Decoder::default(),
            buffer: first.to_vec(),
            start: 0,
        }
    }

    /// Return the next event other than [`Event::NeedData`], passing at most
    /// `limit` bytes of input to the decoder at a time.
    async fn next_event(&mut self, limit: usize) -> Result<Event, V7Error> {
        loop {
            let input = &self.buffer[self.start..];
            let (consumed, event) = self.decoder.decode(&input[..input.len().min(limit)])?;
            self.start += consumed;

            if !matches!(event, Event::NeedData) {
                return Ok(event);
            }

            // Drop consumed input before reading more.
            self.buffer.drain(..self.start);
            self.start = 0;
            self.buffer.reserve(READ_SIZE);

            if self
                .reader
                .read_buf(&mut self.buffer)
                .await
                .map_err(V7Error::Io)?
                == 0
            {
                return Err(self.decoder.eof());
            }
        }
    }

    /// Return the payload consumed by the last [`Event::Payload`].
    fn payload(&self, len: usize) -> &[u8] {
        &self.buffer[self.start - len..self.start]
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
        loop {
            match self.next_event(usize::MAX).await? {
                Event::Member(member) => {
                    return Ok(Some(map_payload(member, |()| V7Payload { archive: self })))
                }
                // Unread payload of the previous member.
                Event::Payload { .. } => (),
                Event::End => return Ok(None),
                Event::NeedData => unreachable!("handled by next_event"),
            }
        }
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
        if !self.archive.decoder.in_payload() {
            return Ok(false);
        }

        match self.archive.next_event(target_len.max(1)).await? {
            Event::Payload { len } => {
                buffer.clear();
                buffer.extend_from_slice(self.archive.payload(len));
                Ok(true)
            }
            // An error was returned before.
            Event::End => Ok(false),
            Event::Member(_) | Event::NeedData => unreachable!("in payload"),
        }
    }

    async fn skip(self) -> Result<(), Self::Error> {
        while self.archive.decoder.in_payload() {
            self.archive.next_event(usize::MAX).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    use futures_util::FutureExt;
    use tar_framing::header::{
        CHECKSUM_RANGE, GID_RANGE, LINK_NAME_RANGE, MODE_RANGE, MTIME_RANGE, SIZE_RANGE,
        TYPEFLAG_OFFSET, UID_RANGE,
    };
    use tokio::io::ReadBuf;

    use super::{header::checksum, *};

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

    /// An archive with members of various sizes.
    fn sample_archive() -> Vec<u8> {
        let mut blocks = file("a", &[1; 700]);
        blocks.push(v7_header("dir/", 0, 0));
        blocks.extend(file("dir/b", &[2; 512]));
        blocks.extend(file("empty", b""));
        blocks.extend(file("c", &[3; 1500]));
        archive(&blocks)
    }

    type Decoded = Vec<(String, Vec<u8>)>;

    /// Drive [`V7Decoder`] synchronously, feeding it `step` bytes at a time.
    fn decode_sync(data: &[u8], step: usize) -> Result<Decoded, V7Error> {
        let mut decoder = V7Decoder::default();
        let mut input = Vec::new();
        let mut fed = 0;
        let mut out: Decoded = Vec::new();

        loop {
            let (consumed, event) = decoder.decode(&input)?;
            match event {
                Event::NeedData => {
                    if fed == data.len() {
                        return Err(decoder.eof());
                    }
                    let n = step.min(data.len() - fed);
                    input.drain(..consumed);
                    input.extend_from_slice(&data[fed..fed + n]);
                    fed += n;
                    continue;
                }
                Event::Member(member) => out.push((member.metadata().path.clone(), Vec::new())),
                Event::Payload { len } => {
                    out.last_mut().unwrap().1.extend_from_slice(&input[..len])
                }
                Event::End => return Ok(out),
            }
            input.drain(..consumed);
        }
    }

    #[test]
    fn decoder_is_incremental() {
        let data = sample_archive();
        let expected = decode_sync(&data, usize::MAX).unwrap();
        assert_eq!(
            expected
                .iter()
                .map(|(path, content)| (path.as_str(), content.len()))
                .collect::<Vec<_>>(),
            [
                ("a", 700),
                ("dir/", 0),
                ("dir/b", 512),
                ("empty", 0),
                ("c", 1500)
            ]
        );

        for step in [1, 7, 511, 512, 513, 4096] {
            assert_eq!(decode_sync(&data, step).unwrap(), expected, "step {step}");
        }

        // Errors do not depend on how the input is split either.
        let truncated = &data[..1000];
        for step in [1, 7, 512, usize::MAX] {
            let err = decode_sync(truncated, step).unwrap_err();
            assert_eq!(
                err.to_string(),
                "at byte 1000: invalid v7 tar archive: truncated member payload",
                "step {step}"
            );
        }
    }

    /// Returns a few bytes at a time, and `Pending` every other poll.
    struct Trickle<'a> {
        data: &'a [u8],
        pending: bool,
        step: usize,
    }

    impl AsyncRead for Trickle<'_> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = &mut *self;
            this.pending = !this.pending;
            if this.pending {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            this.step = this.step % 13 + 1;
            let n = this.step.min(this.data.len()).min(buf.remaining());
            buf.put_slice(&this.data[..n]);
            this.data = &this.data[n..];
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn archive_is_cancel_safe() {
        let data = sample_archive();
        let first: Block = data[..BLOCK_SIZE].try_into().unwrap();
        let reader = Trickle {
            data: &data[BLOCK_SIZE..],
            pending: false,
            step: 0,
        };
        let mut archive = V7Archive::new(reader, first);

        let mut out: Decoded = Vec::new();
        let mut cancelled = 0;
        loop {
            let limit = if archive.decoder.in_payload() {
                100
            } else {
                usize::MAX
            };
            // Poll once, dropping the future if it is not ready.
            let Some(event) = archive.next_event(limit).now_or_never() else {
                cancelled += 1;
                continue;
            };
            match event.unwrap() {
                Event::Member(member) => out.push((member.metadata().path.clone(), Vec::new())),
                Event::Payload { len } => out
                    .last_mut()
                    .unwrap()
                    .1
                    .extend_from_slice(archive.payload(len)),
                Event::End => break,
                Event::NeedData => unreachable!(),
            }
        }

        assert!(cancelled > 100, "{cancelled}");
        assert_eq!(out, decode_sync(&data, usize::MAX).unwrap());
    }
}
