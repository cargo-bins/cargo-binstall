use std::{
    collections::BTreeMap,
    fs,
    io::{self, Seek},
    path::Path,
};

use fs_lock::FileLock;
use miette::Diagnostic;
use thiserror::Error as ThisError;

use crate::{
    binstall_crates_v1::{Error as BinstallCratesV1Error, Records as BinstallCratesV1Records},
    cargo_crates_v1::{CratesToml, CratesTomlParseError},
    crate_info::CrateInfo, helpers::create_if_not_exist, CompactString, Version,
};

#[derive(Debug, Diagnostic, ThisError)]
#[non_exhaustive]
pub enum ManifestsError {
    #[error("failed to parse binstall crates-v1 manifest: {0}")]
    #[diagnostic(transparent)]
    BinstallCratesV1(#[from] BinstallCratesV1Error),

    #[error("failed to parse cargo v1 manifest: {0}")]
    #[diagnostic(transparent)]
    CargoManifestV1(#[from] CratesTomlParseError),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

pub struct Manifests {
    binstall: BinstallCratesV1Records,
    cargo_crates_v1: FileLock,
}

impl Manifests {
    pub fn open_exclusive(cargo_roots: &Path) -> Result<Self, ManifestsError> {
        // Read cargo_binstall_metadata
        let metadata_path = cargo_roots.join("binstall/crates-v1.json");
        fs::create_dir_all(metadata_path.parent().unwrap())?;

        let binstall = BinstallCratesV1Records::load_from_path(&metadata_path)?;

        // Read cargo_install_v1_metadata
        let manifest_path = cargo_roots.join(".crates.toml");

        let cargo_crates_v1 = create_file_if_not_exist(manifest_path)?;

        Ok(Self {
            binstall,
            cargo_crates_v1,
        })
    }

    fn rewind_cargo_crates_v1(&mut self) -> Result<(), ManifestsError> {
        self.cargo_crates_v1.rewind().map_err(ManifestsError::from)
    }

    /// `cargo-uninstall` can be called to uninstall crates,
    /// but it only updates .crates.toml.
    ///
    /// So here we will honour .crates.toml only.
    pub fn load_installed_crates(
        &mut self,
    ) -> Result<BTreeMap<CompactString, Version>, ManifestsError> {
        self.rewind_cargo_crates_v1()?;

        CratesToml::load_from_reader(&mut self.cargo_crates_v1)
            .and_then(CratesToml::collect_into_crates_versions)
            .map_err(ManifestsError::from)
    }

    pub fn update(mut self, metadata_vec: Vec<CrateInfo>) -> Result<(), ManifestsError> {
        self.rewind_cargo_crates_v1()?;

        CratesToml::append_to_file(&mut self.cargo_crates_v1, &metadata_vec)?;

        for metadata in metadata_vec {
            self.binstall.replace(metadata);
        }
        self.binstall.overwrite()?;

        Ok(())
    }
}
