use exn::ResultExt;
use std::path::{Path, PathBuf};

use binstalk_downloader::download::{DownloadError, TarEntriesVisitor, TarEntry};
use binstalk_types::cargo_toml_binstall::Meta;
use cargo_toml_workspace::cargo_toml::{Manifest, Value};
use normalize_path::NormalizePath;
use tokio::io::AsyncReadExt;
use tracing::debug;

use crate::{vfs::Vfs, RegistryError};

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
    async fn visit(&mut self, entry: &mut dyn TarEntry) -> exn::Result<(), DownloadError> {
        let path = entry.path().or_raise(|| DownloadError::Io)?;
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
                    .size()
                    .or_raise(|| DownloadError::Io)?
                    .try_into()
                    .unwrap_or(usize::MAX),
            );
            entry
                .read_to_end(&mut self.cargo_toml_content)
                .await
                .or_raise(|| DownloadError::Io)?;
        }

        Ok(())
    }
}

impl ManifestVisitor {
    /// Load binstall metadata using the extracted information stored in memory.
    pub(super) fn load_manifest(self) -> exn::Result<Manifest<Meta>, RegistryError> {
        debug!("Loading manifest directly from extracted file");

        // Load and parse manifest
        let mut manifest = Manifest::from_slice_with_metadata(&self.cargo_toml_content)
            .or_raise(|| RegistryError::CargoManifest)?;
        debug!("Manifest: {manifest:?}");
        // Checks vfs for binary output names
        manifest
            .complete_from_abstract_filesystem::<Value, _>(&self.vfs, None)
            .or_raise(|| RegistryError::CargoManifest)?;

        // Return metadata
        Ok(manifest)
    }
}
