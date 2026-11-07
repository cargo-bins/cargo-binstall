//! In memory processing of tar archives.

use std::{fmt::Debug, path::Path};

use bytes::Bytes;
use futures_util::Stream;
use tar_codec::{Member, MemberPayload, SpecialKind};
use tracing::debug;

use super::{decode_error, DownloadError, TarArchive, TarBasedFmt, TarPayload, TarReader};

#[async_trait::async_trait]
pub trait TarEntry: Send + Sync + Debug {
    /// Returns the path name for this entry.
    ///
    /// The path is returned as stored in the archive, it is not normalized
    /// and might contain `..` or be absolute.
    fn path(&self) -> &Path;

    /// Returns the size of the content of this entry.
    fn size(&self) -> u64;

    fn entry_type(&self) -> TarEntryType;

    /// Replace the content of `buf` with the content of this entry.
    async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<(), DownloadError>;
}

#[async_trait::async_trait]
impl<T: TarEntry + ?Sized> TarEntry for &mut T {
    fn path(&self) -> &Path {
        T::path(self)
    }

    fn size(&self) -> u64 {
        T::size(self)
    }

    fn entry_type(&self) -> TarEntryType {
        T::entry_type(self)
    }

    async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<(), DownloadError> {
        T::read_to_end(self, buf).await
    }
}

struct Entry<'a, 's> {
    path: String,
    size: u64,
    entry_type: TarEntryType,
    payload: Option<TarPayload<'a, TarReader<'s>>>,
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
    fn path(&self) -> &Path {
        Path::new(&self.path)
    }

    fn size(&self) -> u64 {
        self.size
    }

    fn entry_type(&self) -> TarEntryType {
        self.entry_type
    }

    async fn read_to_end(&mut self, buf: &mut Vec<u8>) -> Result<(), DownloadError> {
        buf.clear();

        let Some(mut payload) = self.payload.take() else {
            return Ok(());
        };
        let remaining = |buf: &Vec<u8>| {
            usize::try_from(self.size)
                .unwrap_or(usize::MAX)
                .saturating_sub(buf.len())
                .max(1)
        };

        // `next_chunk` replaces the content of the buffer instead of
        // appending to it. Ask for the whole content at once, which `tar_codec`
        // returns in a single chunk, so that usually no extra buffer is needed.
        if !payload
            .next_chunk(buf, remaining(buf))
            .await
            .map_err(decode_error)?
        {
            return Ok(());
        }

        let mut chunk = Vec::new();
        while payload
            .next_chunk(&mut chunk, remaining(buf))
            .await
            .map_err(decode_error)?
        {
            buf.extend_from_slice(&chunk);
        }

        Ok(())
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

pub(in crate::download) async fn visit_tar_based_stream<S>(
    stream: S,
    fmt: TarBasedFmt,
    visitor: &mut dyn TarEntriesVisitor,
) -> Result<(), DownloadError>
where
    S: Stream<Item = Result<Bytes, DownloadError>> + Send + Sync,
{
    debug!("Extracting from {fmt} archive to process it in memory");

    let mut archive = TarArchive::open(stream, fmt, |reader| reader).await?;

    while let Some(member) = archive.next_member().await.map_err(decode_error)? {
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
        // `archive.next_member()`.
    }

    Ok(())
}

#[cfg(test)]
mod test {
    use futures_util::stream;

    use super::{super::test::TarBuilder, *};

    #[derive(Default)]
    struct Recorder(Vec<(String, u64, String, Vec<u8>)>);

    #[async_trait::async_trait]
    impl TarEntriesVisitor for Recorder {
        async fn visit(&mut self, entry: &mut dyn TarEntry) -> Result<(), DownloadError> {
            // `read_to_end` must replace the content of the buffer.
            let mut content = b"garbage".to_vec();
            entry.read_to_end(&mut content).await?;
            self.0.push((
                entry.path().to_str().unwrap().to_owned(),
                entry.size(),
                format!("{:?}", entry.entry_type()),
                content,
            ));
            Ok(())
        }
    }

    async fn visit(archive: Bytes) -> Vec<(String, u64, String, Vec<u8>)> {
        let mut recorder = Recorder::default();
        visit_tar_based_stream(stream::iter([Ok(archive)]), TarBasedFmt::Tar, &mut recorder)
            .await
            .unwrap();
        recorder.0
    }

    #[tokio::test]
    async fn visits_entries() {
        let big = vec![b'b'; 200 * 1024];

        for (name, archive) in [
            (
                "ustar",
                TarBuilder::default()
                    .entry("dir/", b'5', "")
                    .file("dir/big", 0o644, &big)
                    .entry("link", b'2', "dir/big")
                    .file("empty", 0o644, b"")
                    .finish(),
            ),
            (
                "v7",
                TarBuilder::default()
                    .v7_file("dir/big", &big)
                    .v7_file("empty", b"")
                    .finish(),
            ),
        ] {
            let entries = visit(archive).await;
            let file = |path: &str, content: &[u8]| {
                (
                    path.to_owned(),
                    content.len() as u64,
                    "Regular".to_owned(),
                    content.to_vec(),
                )
            };

            let expected = if name == "ustar" {
                vec![
                    ("dir/".to_owned(), 0, "Directory".to_owned(), vec![]),
                    file("dir/big", &big),
                    ("link".to_owned(), 0, "Symlink".to_owned(), vec![]),
                    file("empty", b""),
                ]
            } else {
                vec![file("dir/big", &big), file("empty", b"")]
            };
            assert!(entries == expected, "{name}");
        }
    }
}
