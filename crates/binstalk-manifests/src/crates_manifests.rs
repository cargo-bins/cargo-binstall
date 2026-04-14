use std::{
    collections::BTreeMap,
    fs,
    io::{self, Seek},
    path::{Path, PathBuf},
};

use fs_lock::FileLock;
use miette::Diagnostic;
use thiserror::Error as ThisError;

use crate::{
    binstall_crates_v1::{Error as BinstallCratesV1Error, Records as BinstallCratesV1Records},
    cargo_crates_v1::{CratesToml, CratesTomlParseError},
    crate_info::CrateInfo,
    helpers::create_if_not_exist,
    CompactString, Version,
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
    /// Current Cargo root used to resolve tracked extra-file paths.
    ///
    /// The persisted manifest stores extra files relative to Cargo root so
    /// updates can clean up stale files without baking absolute machine-local
    /// paths into the record.
    cargo_root: PathBuf,
    cargo_crates_v1: FileLock,
    installed_crates: BTreeMap<CompactString, Version>,
}

impl Manifests {
    pub fn open_exclusive(cargo_roots: &Path) -> Result<Self, ManifestsError> {
        // Read cargo_binstall_metadata
        let binstall_dir = cargo_roots.join("binstall");
        fs::create_dir_all(&binstall_dir)?;

        let metadata_path = binstall_dir.join("crates-v1.json");

        let mut binstall = BinstallCratesV1Records::load_from_path(&metadata_path)?;

        // Read cargo_install_v1_metadata
        let manifest_path = cargo_roots.join(".crates.toml");

        let mut cargo_crates_v1 = create_if_not_exist(&manifest_path)?;

        let installed_crates = CratesToml::load_from_reader(&mut cargo_crates_v1)
            .and_then(CratesToml::collect_into_crates_versions)?;

        binstall.retain(|crate_info| installed_crates.contains_key(&crate_info.name));

        Ok(Self {
            binstall,
            cargo_root: cargo_roots.to_path_buf(),
            cargo_crates_v1,
            installed_crates,
        })
    }

    fn rewind_cargo_crates_v1(&mut self) -> Result<(), ManifestsError> {
        self.cargo_crates_v1.rewind().map_err(ManifestsError::from)
    }

    /// `cargo-uninstall` can be called to uninstall crates,
    /// but it only updates .crates.toml.
    ///
    /// So here we will honour .crates.toml only.
    pub fn installed_crates(&self) -> &BTreeMap<CompactString, Version> {
        &self.installed_crates
    }

    pub fn update(mut self, metadata_vec: Vec<CrateInfo>) -> Result<(), ManifestsError> {
        self.rewind_cargo_crates_v1()?;

        CratesToml::append_to_file(&mut self.cargo_crates_v1, &metadata_vec)?;

        // Tracking extra files is the most stateful part of this feature, but
        // it is what makes upgrades behave like users expect: if a release
        // stops shipping a completion or moves a man page, the old file should
        // not be left behind indefinitely. We therefore update `.crates.toml`,
        // remove stale tracked extras for the crate, then replace the binstall
        // record with the new relative paths.
        for metadata in metadata_vec {
            // Remove files that this crate used to own before replacing the
            // record. This keeps upgrades idempotent when a maintainer changes
            // completion/manpage paths or stops shipping one of them.
            self.remove_stale_extra_files(&metadata)?;
            self.binstall.replace(metadata);
        }
        self.binstall.overwrite()?;

        Ok(())
    }

    fn remove_stale_extra_files(&self, metadata: &CrateInfo) -> Result<(), ManifestsError> {
        let Some(previous) = self.binstall.get(&metadata.name) else {
            return Ok(());
        };

        // Cleanup is intentionally conservative: only paths previously tracked
        // for the same crate and no longer present in the new record are
        // removed. Missing files are ignored so manual deletion does not block
        // upgrades.
        for extra_file in previous
            .extra_files
            .iter()
            .filter(|path| !metadata.extra_files.contains(path))
        {
            let path = self.cargo_root.join(extra_file);
            match fs::remove_file(&path) {
                Ok(()) => (),
                Err(err) if err.kind() == io::ErrorKind::NotFound => (),
                Err(err) => return Err(ManifestsError::Io(err)),
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        binstall_crates_v1::append_to_path as append_binstall_records, cargo_crates_v1::CratesToml,
        crate_info::CrateSource,
    };
    use detect_targets::TARGET;
    use semver::Version;
    use tempfile::tempdir;

    #[test]
    fn update_removes_stale_extra_files() {
        let cargo_root = tempdir().unwrap();
        let old_extra = PathBuf::from("share/man/man1/cargo-watch.1");
        let new_extra = PathBuf::from("share/man/man1/cargo-watch-new.1");

        fs::create_dir_all(cargo_root.path().join("share/man/man1")).unwrap();
        fs::write(cargo_root.path().join(&old_extra), "old").unwrap();
        fs::write(cargo_root.path().join(&new_extra), "new").unwrap();

        let old_record = CrateInfo {
            name: "cargo-watch".into(),
            version_req: "*".into(),
            current_version: Version::new(8, 4, 0),
            source: CrateSource::cratesio_registry(),
            target: TARGET.into(),
            bins: vec!["cargo-watch".into()],
            extra_files: vec![old_extra.clone()],
        };

        CratesToml::append_to_path(
            cargo_root.path().join(".crates.toml"),
            &[old_record.clone()],
        )
        .unwrap();
        fs::create_dir_all(cargo_root.path().join("binstall")).unwrap();
        append_binstall_records(
            cargo_root.path().join("binstall/crates-v1.json"),
            [old_record],
        )
        .unwrap();

        let manifests = Manifests::open_exclusive(cargo_root.path()).unwrap();
        manifests
            .update(vec![CrateInfo {
                name: "cargo-watch".into(),
                version_req: "*".into(),
                current_version: Version::new(8, 5, 0),
                source: CrateSource::cratesio_registry(),
                target: TARGET.into(),
                bins: vec!["cargo-watch".into()],
                extra_files: vec![new_extra.clone()],
            }])
            .unwrap();

        assert!(!cargo_root.path().join(old_extra).exists());
        assert!(cargo_root.path().join(new_extra).exists());
    }
}
