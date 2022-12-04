use std::{
    io,
    path::{Path, PathBuf},
};

use cargo_toml::Manifest;
use normalize_path::NormalizePath;
use tokio::io::AsyncReadExt;
use tracing::debug;

use super::vfs::Vfs;
use crate::{
    errors::BinstallError,
    helpers::download::{DownloadError, TarEntriesVisitor, TarEntry},
    manifests::cargo_toml_binstall::Meta,
};

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
    type Target = Manifest<Meta>;

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

        Ok(())
    }

    /// Load binstall metadata using the extracted information stored in memory.
    fn finish(self) -> Result<Self::Target, DownloadError> {
        Ok(load_manifest(&self.cargo_toml_content, &self.vfs).map_err(io::Error::from)?)
    }
}

fn load_manifest(slice: &[u8], vfs: &Vfs) -> Result<Manifest<Meta>, BinstallError> {
    debug!("Loading manifest directly from extracted file");

    // Load and parse manifest
    let mut manifest = Manifest::from_slice_with_metadata(slice)?;

    // Checks vfs for binary output names
    manifest.complete_from_abstract_filesystem(vfs)?;

    // Return metadata
    Ok(manifest)
}
