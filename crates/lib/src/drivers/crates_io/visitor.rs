use std::{
    io::Read,
    path::{Path, PathBuf},
};

use cargo_toml::Manifest;
use log::debug;
use tar::Entries;

use super::vfs::Vfs;
use crate::{
    errors::BinstallError,
    helpers::{PathExt, TarEntriesVisitor},
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
            vfs: Vfs::new(),
        }
    }
}

impl TarEntriesVisitor for ManifestVisitor {
    type Target = Manifest<Meta>;

    fn visit<R: Read>(&mut self, entries: Entries<'_, R>) -> Result<(), BinstallError> {
        for res in entries {
            let mut entry = res?;
            let path = entry.path()?;
            let path = path.normalize_path();

            let path = if let Ok(path) = path.strip_prefix(&self.manifest_dir_path) {
                path
            } else {
                // The path is outside of the curr dir (manifest dir),
                // ignore it.
                continue;
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
                entry.read_to_end(&mut self.cargo_toml_content)?;
            }
        }

        Ok(())
    }

    /// Load binstall metadata using the extracted information stored in memory.
    fn finish(self) -> Result<Self::Target, BinstallError> {
        debug!("Loading manifest directly from extracted file");

        // Load and parse manifest
        let mut manifest = Manifest::from_slice_with_metadata(&self.cargo_toml_content)?;

        // Checks vfs for binary output names
        manifest.complete_from_abstract_filesystem(&self.vfs)?;

        // Return metadata
        Ok(manifest)
    }
}
