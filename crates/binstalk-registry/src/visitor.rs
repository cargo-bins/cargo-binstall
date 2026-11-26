use std::path::{Path, PathBuf};

use binstalk_downloader::download::{DownloadError, TarEntriesVisitor, TarEntry};
use binstalk_types::cargo_toml_binstall::Meta;
use cargo_toml_workspace::cargo_toml::{Manifest, Value};
use normalize_path::NormalizePath;
use tokio::io::AsyncReadExt;
use tracing::debug;

use crate::{
    RegistryError,
    dist::{DIST_CONFIG_FILES, MAX_CONFIG_FILE_SIZE, WorkspaceDistConfig},
    vfs::Vfs,
};

#[derive(Debug)]
pub(super) struct ManifestVisitor {
    cargo_toml_content: Vec<u8>,
    /// manifest_dir_path is treated as the current dir.
    manifest_dir_path: PathBuf,

    vfs: Vfs,

    /// For the `[package.metadata.dist]` that [`ManifestVisitor::load_manifest`]
    /// returns.
    dist_config: WorkspaceDistConfig,
}

impl ManifestVisitor {
    pub(super) fn new(manifest_dir_path: PathBuf) -> Self {
        Self {
            // Cargo.toml is quite large usually.
            cargo_toml_content: Vec::with_capacity(2000),
            manifest_dir_path,
            vfs: Vfs::default(),
            dist_config: WorkspaceDistConfig::default(),
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
            self.cargo_toml_content
                .reserve_exact(entry.size()?.try_into().unwrap_or(usize::MAX));
            entry.read_to_end(&mut self.cargo_toml_content).await?;
        }

        let dist_config = if path == Path::new("Cargo.toml.orig") {
            Some(&mut self.dist_config.root_manifest)
        } else {
            DIST_CONFIG_FILES
                .iter()
                .position(|name| path == Path::new(name))
                .map(|i| &mut self.dist_config.files[i])
        };
        if let Some(dist_config) = dist_config {
            let size = entry.size()?;
            if size <= MAX_CONFIG_FILE_SIZE {
                let mut content = Vec::with_capacity(size.try_into().unwrap_or(0));
                entry.read_to_end(&mut content).await?;
                *dist_config = Some(content);
            } else {
                debug!(
                    "Ignoring {}: larger than {MAX_CONFIG_FILE_SIZE} bytes",
                    path.display()
                );
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
        self.dist_config.merge_into(&mut manifest);

        // Return metadata
        Ok(manifest)
    }
}
