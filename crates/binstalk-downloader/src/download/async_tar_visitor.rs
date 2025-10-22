use std::{borrow::Cow, fmt::Debug, io, path::Path, pin::Pin};

use async_compression::tokio::bufread;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::io::{copy, sink, AsyncRead};
use tokio_tar::{Archive, Entry, EntryType};
use tokio_util::io::StreamReader;
use tracing::debug;

use super::{
    DownloadError,
    TarBasedFmt::{self, *},
};

pub trait TarEntry: AsyncRead + Send + Sync + Unpin + Debug {
    /// Returns the path name for this entry.
    ///
    /// This method may fail if the pathname is not valid Unicode and
    /// this is called on a Windows platform.
    ///
    /// Note that this function will convert any `\` characters to
    /// directory separators.
    fn path(&self) -> io::Result<Cow<'_, Path>>;

    fn size(&self) -> io::Result<u64>;

    fn entry_type(&self) -> TarEntryType;
}

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
}

impl<R: AsyncRead + Unpin + Send + Sync> TarEntry for Entry<R> {
    fn path(&self) -> io::Result<Cow<'_, Path>> {
        Entry::path(self)
    }

    fn size(&self) -> io::Result<u64> {
        self.header().size()
    }

    fn entry_type(&self) -> TarEntryType {
        match self.header().entry_type() {
            EntryType::Regular => TarEntryType::Regular,
            EntryType::Link => TarEntryType::Link,
            EntryType::Symlink => TarEntryType::Symlink,
            EntryType::Char => TarEntryType::Char,
            EntryType::Block => TarEntryType::Block,
            EntryType::Directory => TarEntryType::Directory,
            EntryType::Fifo => TarEntryType::Fifo,
            // Implementation-defined ‘high-performance’ type, treated as regular file
            EntryType::Continuous => TarEntryType::Regular,
            _ => TarEntryType::Unknown,
        }
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

pub(crate) async fn extract_tar_based_stream_and_visit<S>(
    stream: S,
    fmt: TarBasedFmt,
    visitor: &mut dyn TarEntriesVisitor,
) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync,
{
    debug!("Extracting from {fmt} archive to process it in memory");

    let reader = StreamReader::new(stream);
    let decoder: Pin<Box<dyn AsyncRead + Send + Sync>> = match fmt {
        Tar => Box::pin(reader),
        Tbz2 => Box::pin(bufread::BzDecoder::new(reader)),
        Tgz => Box::pin(bufread::GzipDecoder::new(reader)),
        Txz => Box::pin(bufread::XzDecoder::new(reader)),
        Tzstd => Box::pin(bufread::ZstdDecoder::new(reader)),
    };

    let mut tar = Archive::new(decoder);
    let mut entries = tar.entries()?;

    let mut sink = sink();

    while let Some(res) = entries.next().await {
        let mut entry = res?;
        visitor.visit(&mut entry).await?;

        // Consume all remaining data so that next iteration would work fine
        // instead of reading the data of previous entry.
        copy(&mut entry, &mut sink).await?;
    }

    Ok(())
}
