use std::{
    io,
    path::{Path, PathBuf},
};

use binstalk_downloader::download::{DownloadError, TarEntriesVisitor, TarEntry};
use binstalk_types::cargo_toml_binstall::Meta;
use cargo_toml_workspace::cargo_toml::{Manifest, Value};
use normalize_path::NormalizePath;
use tokio::io::AsyncReadExt;
use tracing::debug;

use crate::{RegistryError, vfs::Vfs};

/// The most of a crate's `Cargo.toml` that is read into memory.
///
/// A normalised `Cargo.toml` is kilobytes even for crates with thousands of
/// features, and the tarball's checksum is only checked once all of it has
/// been read, so the size in its tar header can't be trusted to size the
/// buffer by.
const MAX_CARGO_TOML_SIZE: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct ManifestVisitor {
    cargo_toml_content: Vec<u8>,
    /// manifest_dir_path is treated as the current dir.
    manifest_dir_path: PathBuf,

    vfs: Vfs,
}

impl ManifestVisitor {
    pub(super) fn new(manifest_dir_path: PathBuf) -> Self {
        Self {
            // Cargo.toml is quite large usually.
            cargo_toml_content: Vec::with_capacity(2000),
            manifest_dir_path,
            vfs: Vfs::default(),
        }
    }
}

#[async_trait::async_trait]
impl TarEntriesVisitor for ManifestVisitor {
    async fn visit(&mut self, entry: &mut dyn TarEntry) -> Result<(), DownloadError> {
        let path = entry.path()?;
        let path = path.normalize();

        let path = if let Ok(path) = path.strip_prefix(&self.manifest_dir_path) {
            path
        } else {
            // The path is outside of the curr dir (manifest dir),
            // ignore it.
            return Ok(());
        };

        if path == Path::new("Cargo.toml")
            || path == Path::new("src/main.rs")
            || path.starts_with("src/bin")
        {
            self.vfs.add_path(path);
        }

        if path == Path::new("Cargo.toml") {
            // Since it is possible for the same Cargo.toml to appear
            // multiple times using `tar --keep-old-files`, here we
            // clear the buffer first before reading into it.
            self.cargo_toml_content.clear();
            self.cargo_toml_content.reserve_exact(
                entry
                    .size()?
                    .min(MAX_CARGO_TOML_SIZE)
                    .try_into()
                    .unwrap_or(usize::MAX),
            );
            entry
                .take(MAX_CARGO_TOML_SIZE + 1)
                .read_to_end(&mut self.cargo_toml_content)
                .await?;
            if self.cargo_toml_content.len() as u64 > MAX_CARGO_TOML_SIZE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Cargo.toml is larger than {MAX_CARGO_TOML_SIZE} bytes"),
                )
                .into());
            }
        }

        Ok(())
    }
}

impl ManifestVisitor {
    /// Load binstall metadata using the extracted information stored in memory.
    pub(super) fn load_manifest(self) -> Result<Manifest<Meta>, RegistryError> {
        debug!("Loading manifest directly from extracted file");

        // Load and parse manifest
        let mut manifest = Manifest::from_slice_with_metadata(&self.cargo_toml_content)?;
        debug!("Manifest: {manifest:?}");
        // Checks vfs for binary output names
        manifest.complete_from_abstract_filesystem::<Value, _>(&self.vfs, None)?;

        // Return metadata
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        pin::Pin,
        task::{Context, Poll},
    };

    use binstalk_downloader::download::TarEntryType;
    use tokio::io::{AsyncRead, ReadBuf};

    use super::*;

    /// A `Cargo.toml` entry whose header claims `size` bytes and whose body
    /// is `left` bytes of `#`, a TOML comment.
    #[derive(Debug)]
    struct Entry {
        size: u64,
        left: u64,
    }

    impl AsyncRead for Entry {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let n = buf
                .remaining()
                .min(self.left.try_into().unwrap_or(usize::MAX));
            buf.put_slice(&vec![b'#'; n]);
            self.left -= n as u64;
            Poll::Ready(Ok(()))
        }
    }

    impl TarEntry for Entry {
        fn path(&self) -> io::Result<Cow<'_, Path>> {
            Ok(Path::new("krate-1.0.0/Cargo.toml").into())
        }

        fn size(&self) -> io::Result<u64> {
            Ok(self.size)
        }

        fn entry_type(&self) -> TarEntryType {
            TarEntryType::Regular
        }
    }

    async fn visit(size: u64, len: u64) -> Result<usize, DownloadError> {
        let mut visitor = ManifestVisitor::new("krate-1.0.0".into());
        visitor.visit(&mut Entry { size, left: len }).await?;
        Ok(visitor.cargo_toml_content.len())
    }

    #[tokio::test]
    async fn header_size_is_not_trusted() {
        // Reserving this much up front would abort the process.
        assert_eq!(visit(u64::MAX, 10).await.unwrap(), 10);
    }

    #[tokio::test]
    async fn oversized_cargo_toml_is_rejected() {
        assert_eq!(
            visit(0, MAX_CARGO_TOML_SIZE).await.unwrap() as u64,
            MAX_CARGO_TOML_SIZE
        );
        visit(0, MAX_CARGO_TOML_SIZE + 1).await.unwrap_err();
        visit(0, u64::MAX).await.unwrap_err();
    }
}
