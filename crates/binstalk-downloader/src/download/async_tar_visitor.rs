use std::{borrow::Cow, fmt::Debug, future::Future, io, path::Path, pin::Pin};

use bytes::Bytes;
use futures_util::Stream;
use tar_codec::{
    DecodeError, LentPayload, Member, MemberPayload, Members, SpecialKind, TarArchive,
    TarMemberPayload,
};
use tracing::debug;

use super::{
    tar::{create_tar_archive, create_tar_decoder, decode_error, TarDecoder},
    DownloadError, TarBasedFmt,
};

#[async_trait::async_trait]
pub trait TarEntry: Send + Sync + Debug {
    /// Returns the path name for this entry.
    ///
    /// The path is returned as stored in the archive, it is not normalized
    /// and might contain `..` or be absolute.
    fn path(&self) -> io::Result<Cow<'_, Path>>;

    /// Returns the size of the content of this entry.
    fn size(&self) -> io::Result<u64>;

    fn entry_type(&self) -> TarEntryType;

    /// Read all remaining content of this entry and append it to `buf`.
    ///
    /// Returns the number of bytes appended.
    async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize, DownloadError>;
}

#[async_trait::async_trait]
impl<T: TarEntry + ?Sized> TarEntry for &mut T {
    fn path(&self) -> io::Result<Cow<'_, Path>> {
        T::path(self)
    }

    fn size(&self) -> io::Result<u64> {
        T::size(self)
    }

    fn entry_type(&self) -> TarEntryType {
        T::entry_type(self)
    }

    async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize, DownloadError> {
        T::read_to_end(self, buf).await
    }
}

/// Chunk size used when reading the content of an entry.
const CHUNK_SIZE: usize = 64 * 1024;

type Payload<'a, 's> = LentPayload<'a, TarMemberPayload<'a, TarDecoder<'s>>>;

struct Entry<'a, 's> {
    path: String,
    size: u64,
    entry_type: TarEntryType,
    payload: Option<Payload<'a, 's>>,
}

impl Debug for Entry<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("path", &self.path)
            .field("size", &self.size)
            .field("entry_type", &self.entry_type)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl TarEntry for Entry<'_, '_> {
    fn path(&self) -> io::Result<Cow<'_, Path>> {
        Ok(Cow::Borrowed(Path::new(&self.path)))
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.size)
    }

    fn entry_type(&self) -> TarEntryType {
        self.entry_type
    }

    async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<usize, DownloadError> {
        let Some(payload) = self.payload.as_mut() else {
            return Ok(0);
        };

        let mut chunk = Vec::new();
        let mut n = 0;

        while payload
            .next_chunk(&mut chunk, CHUNK_SIZE)
            .await
            .map_err(decode_error)?
        {
            buf.extend_from_slice(&chunk);
            n += chunk.len();
        }

        // The payload is fully consumed.
        self.payload = None;

        Ok(n)
    }
}

#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
pub enum TarEntryType {
    Regular,
    Link,
    Symlink,
    Char,
    Block,
    Directory,
    Fifo,
    Unknown,
}

/// Visitor must iterate over all entries.
/// Entries can be in arbitrary order.
#[async_trait::async_trait]
pub trait TarEntriesVisitor: Send + Sync {
    /// Will be called once per entry
    async fn visit(&mut self, entry: &mut dyn TarEntry) -> Result<(), DownloadError>;
}

pub(crate) fn extract_tar_based_stream_and_visit<'a, S>(
    stream: S,
    fmt: TarBasedFmt,
    visitor: &'a mut dyn TarEntriesVisitor,
) -> Pin<Box<dyn Future<Output = Result<(), DownloadError>> + Send + 'a>>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync + 'a,
{
    // The future is boxed here so that it is proven to be `Send` with
    // concrete lifetimes, otherwise rustc fails with "implementation of
    // `Send` is not general enough" in callers, see rust-lang/rust#110338.
    Box::pin(visit_tar_based_stream(stream, fmt, visitor))
}

type NextMember<'a, 's> =
    Pin<Box<dyn Future<Output = Result<Option<Member<Payload<'a, 's>>>, DecodeError>> + Send + 'a>>;

/// Box the future returned by [`Members::next`] with concrete lifetimes so
/// that rustc can prove it is `Send`.
fn next_member<'a, 's>(members: &'a mut Members<TarArchive<TarDecoder<'s>>>) -> NextMember<'a, 's> {
    Box::pin(members.next())
}

async fn visit_tar_based_stream<S>(
    stream: S,
    fmt: TarBasedFmt,
    visitor: &mut dyn TarEntriesVisitor,
) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync,
{
    debug!("Extracting from {fmt} archive to process it in memory");

    let mut members =
        tar_codec::Archive::members(create_tar_archive(create_tar_decoder(stream, fmt)));

    while let Some(member) = next_member(&mut members).await.map_err(decode_error)? {
        let (metadata, size, entry_type, payload) = match member {
            Member::File {
                metadata,
                size,
                payload,
                ..
            } => (metadata, size, TarEntryType::Regular, Some(payload)),
            Member::HardLink {
                metadata,
                size,
                payload,
                ..
            } => (metadata, size, TarEntryType::Link, Some(payload)),
            Member::Directory { metadata } => (metadata, 0, TarEntryType::Directory, None),
            Member::SymbolicLink { metadata, .. } => (metadata, 0, TarEntryType::Symlink, None),
            Member::Special { metadata, kind } => (
                metadata,
                0,
                match kind {
                    SpecialKind::CharacterDevice => TarEntryType::Char,
                    SpecialKind::BlockDevice => TarEntryType::Block,
                    SpecialKind::Fifo => TarEntryType::Fifo,
                },
                None,
            ),
        };

        let mut entry = Entry {
            path: metadata.path,
            size,
            entry_type,
            payload,
        };
        visitor.visit(&mut entry).await?;

        // Any remaining content of the entry is drained and validated by
        // `members.next()`.
    }

    Ok(())
}
