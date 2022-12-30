use std::{collections::BTreeMap, fs, io::Seek, path::Path};

use binstalk::errors::BinstallError;
use binstalk_manifests::{
    binstall_crates_v1::Records as BinstallCratesV1Records, cargo_crates_v1::CratesToml,
    crate_info::CrateInfo, CompactString, Version,
};
use fs_lock::FileLock;
use miette::{Error, Result};
use tracing::debug;

pub struct Manifests {
    binstall: BinstallCratesV1Records,
    cargo_crates_v1: FileLock,
}

impl Manifests {
    pub fn open_exlusive(cargo_roots: &Path) -> Result<Self> {
        // Read cargo_binstall_metadata
        let metadata_path = cargo_roots.join("binstall/crates-v1.json");
        fs::create_dir_all(metadata_path.parent().unwrap()).map_err(BinstallError::Io)?;

        debug!(
            "Reading {} from {} and obtaining exclusive lock",
            "binstall metadata",
            metadata_path.display()
        );

        let binstall = BinstallCratesV1Records::load_from_path(&metadata_path)?;

        // Read cargo_install_v1_metadata
        let manifest_path = cargo_roots.join(".crates.toml");

        debug!(
            "Obtaining exclusive lock of {} in path {}",
            "cargo install v1 metadata",
            manifest_path.display()
        );

        let cargo_crates_v1 = fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .open(manifest_path)
            .and_then(FileLock::new_exclusive)
            .map_err(BinstallError::Io)?;

        Ok(Self {
            binstall,
            cargo_crates_v1,
        })
    }

    fn rewind_cargo_crates_v1(&mut self) -> Result<()> {
        self.cargo_crates_v1
            .rewind()
            .map_err(BinstallError::Io)
            .map_err(Error::from)
    }

    pub fn load_installed_crates(&mut self) -> Result<BTreeMap<CompactString, Version>> {
        self.rewind_cargo_crates_v1()?;

        CratesToml::load_from_reader(&mut self.cargo_crates_v1)
            .and_then(CratesToml::collect_into_crates_versions)
            .map_err(Error::from)
    }

    pub fn update(mut self, metadata_vec: Vec<CrateInfo>) -> Result<()> {
        self.rewind_cargo_crates_v1()?;

        debug!("Writing .crates.toml");
        CratesToml::append_to_file(&mut self.cargo_crates_v1, &metadata_vec)?;

        debug!("Writing binstall/crates-v1.json");
        for metadata in metadata_vec {
            self.binstall.replace(metadata);
        }
        self.binstall.overwrite()?;

        Ok(())
    }
}
