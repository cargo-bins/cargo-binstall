//! Tar decoding built on [`tar_codec`], for extraction ([`extract`]) and in
//! memory processing ([`visitor`]).
//!
//! `tar_codec` is a strict tar decoder that rejects ambiguous archives
//! (e.g. mixed GNU/pax framing, conflicting extension state) which other tar
//! implementations may interpret differently, and its extraction is
//! capability-relative (via `cap-std`), so that no member can escape the
//! extraction root through `..`, absolute paths or pre-existing links.
//!
//! Legacy v7 archives, which `tar_codec` deliberately does not support, are
//! decoded by [`v7`] instead. Both are hidden behind [`TarArchive`], the only
//! [`Archive`] implementation here.

use std::{
    io,
    pin::Pin,
    sync::{Arc, Mutex},
};

use async_compression::tokio::bufread;
use bytes::Bytes;
use futures_util::{Stream, TryStreamExt};
use tar_codec::{
    Archive, DecodeError, DecodePolicy, ExtractError, Member, MemberPayload, PaxDecodePolicy,
    PaxVendorExtensionPolicy, TarArchive as CodecArchive, TarMemberPayload,
};
use tar_framing::{Block, FrameError, FrameErrorInner, BLOCK_SIZE};
use thiserror::Error as ThisError;
use tokio::io::{AsyncRead, AsyncReadExt, Chain};
use tokio_util::io::StreamReader;
use tracing::debug;

use super::{
    DownloadError, ExtractedFiles,
    TarBasedFmt::{self, *},
};

mod extract;
pub(super) use extract::extract_tar_based_stream;
use extract::ExtractReader;

mod v7;
use v7::{V7Error, V7Reader};

mod visitor;
pub(super) use visitor::visit_tar_based_stream;
pub use visitor::{TarEntriesVisitor, TarEntry, TarEntryType};

/// Error returned when decoding or extracting a tar archive fails.
#[derive(Debug, ThisError)]
#[error(transparent)]
pub struct TarError(TarErrorInner);

#[derive(Debug, ThisError)]
enum TarErrorInner {
    #[error(transparent)]
    Decode(TarDecodeError),

    #[error(transparent)]
    Extract(ExtractError<TarDecodeError>),
}

/// Error returned by [`TarArchive`].
#[derive(Debug, ThisError)]
enum TarDecodeError {
    #[error(transparent)]
    Codec(DecodeError),

    #[error(transparent)]
    V7(V7Error),
}

/// Convert a decoding error into [`DownloadError`], unwrapping I/O errors
/// (e.g. from decompression).
fn decode_error(err: TarDecodeError) -> DownloadError {
    match err {
        TarDecodeError::Codec(DecodeError::Framing(FrameError {
            inner: FrameErrorInner::Io { source },
            ..
        }))
        | TarDecodeError::V7(V7Error::Io(source)) => DownloadError::from(source),
        err => DownloadError::Tar(TarError(TarErrorInner::Decode(err))),
    }
}

/// Decompressed tar stream.
type TarReader<'s> = Pin<Box<dyn AsyncRead + Send + Sync + 's>>;

/// The reader of `tar_codec`: the first block, already read to detect the
/// format, followed by the rest of the stream.
type CodecReader<R> = Chain<io::Cursor<Vec<u8>>, R>;

enum Decoder<R> {
    Codec(Box<CodecArchive<CodecReader<R>>>),
    V7(V7Reader<R>),
}

/// A tar archive read from `R`, decoded by `tar_codec` or by [`V7Reader`].
///
/// In memory processing uses the inherent methods, directly on the borrowed
/// download stream.
///
/// [`Archive`], needed by [`Archive::extract_in`], is only implemented for
/// `TarArchive<ExtractReader>`: rustc is currently unable to prove that
/// futures using an archive through [`Archive`] are `Send` when the archive
/// type contains any lifetime, even `'static` (it fails with "implementation
/// of `Archive` is not general enough", see rust-lang/rust#110338). So
/// extraction decodes from a channel, through the lifetime-free
/// [`ExtractReader`].
struct TarArchive<R> {
    decoder: Decoder<R>,
    /// Set when extracting, see [`extract::filter_member`].
    extracted_files: Option<Arc<Mutex<ExtractedFiles>>>,
}

enum TarPayload<'a, R> {
    Codec(TarMemberPayload<'a, CodecReader<R>>),
    V7(&'a mut V7Reader<R>),
}

impl<R: AsyncRead + Unpin> TarArchive<R> {
    /// Decompress `stream` according to `fmt`, and detect the format of the
    /// archive from its first block.
    async fn open<'s, S, E>(
        stream: S,
        fmt: TarBasedFmt,
        wrap: impl FnOnce(TarReader<'s>) -> R,
    ) -> io::Result<Self>
    where
        S: Stream<Item = Result<Bytes, E>> + Send + Sync + 's,
        E: Into<io::Error> + 's,
    {
        let reader = StreamReader::new(stream.map_err(Into::into));
        let reader: TarReader<'s> = match fmt {
            Tar => Box::pin(reader),
            Tbz2 => Box::pin(bufread::BzDecoder::new(reader)),
            Tgz => Box::pin(bufread::GzipDecoder::new(reader)),
            Txz => Box::pin(bufread::XzDecoder::new(reader)),
            Tzstd => Box::pin(bufread::ZstdDecoder::new(reader)),
        };

        let mut reader = wrap(reader);
        let mut first: Block = [0; BLOCK_SIZE];
        let mut filled = 0;
        while filled < BLOCK_SIZE {
            let n = reader.read(&mut first[filled..]).await?;
            if n == 0 {
                break;
            }
            filled += n;
        }

        let decoder = if filled == BLOCK_SIZE && v7::is_v7_header(&first) {
            debug!("Detected legacy v7 tar archive");
            Decoder::V7(V7Reader::new(reader, first))
        } else {
            let reader = io::Cursor::new(first[..filled].to_vec()).chain(reader);
            Decoder::Codec(Box::new(
                CodecArchive::new(reader).with_policy(
                    DecodePolicy::default()
                        // CPython's tarfile is known to fill uname/gname without the
                        // NUL terminator. These fields are never used by binstall.
                        .allow_header_uname_and_gname_without_terminator(true)
                        .pax_policy(PaxDecodePolicy::default().vendor_extension_policy(
                            // bsdtar (default `tar` on macOS) and star record xattrs, ACLs
                            // and file flags under these vendor namespaces. They carry no
                            // information affecting member framing, path or contents, so
                            // they are safe to ignore.
                            //
                            // Other vendor records (e.g. `GNU.sparse.*`) are still
                            // rejected since ignoring them would change the extracted
                            // contents.
                            PaxVendorExtensionPolicy::ignore(["LIBARCHIVE", "SCHILY"]),
                        )),
                ),
            ))
        };

        Ok(Self {
            decoder,
            extracted_files: None,
        })
    }
}

fn map_payload<P, Q>(member: Member<P>, f: impl FnOnce(P) -> Q) -> Member<Q> {
    match member {
        Member::File {
            metadata,
            size,
            executable,
            payload,
        } => Member::File {
            metadata,
            size,
            executable,
            payload: f(payload),
        },
        Member::HardLink {
            metadata,
            target,
            size,
            payload,
        } => Member::HardLink {
            metadata,
            target,
            size,
            payload: f(payload),
        },
        Member::Directory { metadata } => Member::Directory { metadata },
        Member::SymbolicLink { metadata, target } => Member::SymbolicLink { metadata, target },
        Member::Special { metadata, kind } => Member::Special { metadata, kind },
    }
}

impl<R: AsyncRead + Unpin> TarArchive<R> {
    /// Return the next member, after skipping the unread payload of the
    /// current one.
    async fn next_member(&mut self) -> Result<Option<Member<TarPayload<'_, R>>>, TarDecodeError> {
        let member = match &mut self.decoder {
            Decoder::Codec(archive) => archive
                .next_member()
                .await
                .map_err(TarDecodeError::Codec)?
                .map(|member| map_payload(member, TarPayload::Codec)),
            Decoder::V7(reader) => reader
                .next_member()
                .await
                .map_err(TarDecodeError::V7)?
                .map(|member| map_payload(member, |()| TarPayload::V7(reader))),
        };

        Ok(match &self.extracted_files {
            Some(extracted_files) => {
                member.map(|member| extract::filter_member(extracted_files, member))
            }
            None => member,
        })
    }
}

impl Archive for TarArchive<ExtractReader> {
    type Error = TarDecodeError;
    type Payload<'a> = TarPayload<'a, ExtractReader>;

    async fn next_member<'a>(
        &'a mut self,
    ) -> Result<Option<Member<Self::Payload<'a>>>, Self::Error> {
        TarArchive::next_member(self).await
    }
}

impl<R: AsyncRead + Unpin> MemberPayload for TarPayload<'_, R> {
    type Error = TarDecodeError;

    async fn next_chunk(
        &mut self,
        buffer: &mut Vec<u8>,
        target_len: usize,
    ) -> Result<bool, Self::Error> {
        match self {
            Self::Codec(payload) => payload
                .next_chunk(buffer, target_len)
                .await
                .map_err(TarDecodeError::Codec),
            Self::V7(reader) => reader
                .next_chunk(buffer, target_len)
                .await
                .map_err(TarDecodeError::V7),
        }
    }

    async fn skip(self) -> Result<(), Self::Error> {
        match self {
            Self::Codec(payload) => payload.skip().await.map_err(TarDecodeError::Codec),
            Self::V7(reader) => reader.skip_payload().await.map_err(TarDecodeError::V7),
        }
    }
}

#[cfg(test)]
mod test {
    use bytes::Bytes;

    /// Minimal ustar archive writer for tests.
    #[derive(Default)]
    pub(super) struct TarBuilder(Vec<u8>);

    impl TarBuilder {
        fn header(&mut self, name: &str, typeflag: u8, mode: u32, size: usize, link: &str) {
            let mut header = [0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
            header[108..116].copy_from_slice(b"0000000\0");
            header[116..124].copy_from_slice(b"0000000\0");
            header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
            header[136..148].copy_from_slice(b"00000000000\0");
            header[156] = typeflag;
            header[157..157 + link.len()].copy_from_slice(link.as_bytes());
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");

            header[148..156].copy_from_slice(b"        ");
            let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
            header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());

            self.0.extend_from_slice(&header);
        }

        fn data(&mut self, data: &[u8]) {
            self.0.extend_from_slice(data);
            self.0.resize(self.0.len().next_multiple_of(512), 0);
        }

        pub(super) fn file(mut self, name: &str, mode: u32, data: &[u8]) -> Self {
            self.header(name, b'0', mode, data.len(), "");
            self.data(data);
            self
        }

        /// Add a regular file using a legacy v7 header.
        pub(super) fn v7_file(mut self, name: &str, data: &[u8]) -> Self {
            self.header(name, 0, 0o755, data.len(), "");
            let start = self.0.len() - 512;
            let header = &mut self.0[start..];
            header[257..265].fill(0);
            header[148..156].copy_from_slice(b"        ");
            let checksum: u32 = header.iter().map(|b| u32::from(*b)).sum();
            header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
            self.data(data);
            self
        }

        pub(super) fn entry(mut self, name: &str, typeflag: u8, link: &str) -> Self {
            self.header(name, typeflag, 0o755, 0, link);
            self
        }

        pub(super) fn pax(mut self, records: &[(&str, &str)]) -> Self {
            let mut data = Vec::new();
            for (key, value) in records {
                // "<len> <key>=<value>\n", where len includes itself.
                let rest = format!(" {key}={value}\n");
                let mut len = rest.len() + 1;
                while format!("{len}").len() + rest.len() != len {
                    len += 1;
                }
                data.extend_from_slice(format!("{len}{rest}").as_bytes());
            }
            self.header("PaxHeaders/entry", b'x', 0o644, data.len(), "");
            self.data(&data);
            self
        }

        pub(super) fn finish(mut self) -> Bytes {
            self.0.extend_from_slice(&[0; 1024]);
            self.0.into()
        }
    }
}
