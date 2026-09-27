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
    cargo_crates_v1::{CratesToml, CratesTomlParseError, Source},
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InstalledMethod {
    Binstall,
    CargoInstall,
    LocalPath,
    Git,
    Drifted {
        binstall_version: Version,
        cargo_version: Version,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledCrateInfo {
    pub name: CompactString,
    pub cargo_version: Version,
    pub binstall_version: Option<Version>,
    pub method: InstalledMethod,
    pub bins: Vec<CompactString>,
}

pub struct Manifests {
    binstall: BinstallCratesV1Records,
    cargo_crates_v1: FileLock,
    installed_crates: BTreeMap<CompactString, Version>,
    installed_crates_details:
        BTreeMap<CompactString, (Version, Source<'static>, Vec<CompactString>)>,
}

impl Manifests {
    pub fn open_exclusive(cargo_roots: &Path) -> Result<Self, ManifestsError> {
        // Read cargo_binstall_metadata
        let binstall_dir = cargo_roots.join("binstall");
        fs::create_dir_all(&binstall_dir)?;

        let metadata_path = binstall_dir.join("crates-v1.json");

        let binstall = BinstallCratesV1Records::load_from_path(&metadata_path)?;

        // Read cargo_install_v1_metadata
        let manifest_path = cargo_roots.join(".crates.toml");

        let mut cargo_crates_v1 = create_if_not_exist(&manifest_path)?;

        let (installed_crates, installed_crates_details) =
            CratesToml::load_from_reader(&mut cargo_crates_v1)
                .and_then(CratesToml::collect_into_crates_details)?;

        Ok(Self {
            binstall,
            cargo_crates_v1,
            installed_crates,
            installed_crates_details,
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

    pub fn list_crates(&self) -> Vec<InstalledCrateInfo> {
        self.installed_crates_details
            .iter()
            .map(|(name, (cargo_version, source, bins))| {
                let binstall_info = self.binstall.get(name);
                let binstall_version = binstall_info.map(|info| info.current_version.clone());

                let method = match source {
                    Source::Path(_) => InstalledMethod::LocalPath,
                    Source::Git(_) => InstalledMethod::Git,
                    _ => match &binstall_version {
                        None => InstalledMethod::CargoInstall,
                        Some(b_ver) if b_ver == cargo_version => InstalledMethod::Binstall,
                        Some(b_ver) => InstalledMethod::Drifted {
                            binstall_version: b_ver.clone(),
                            cargo_version: cargo_version.clone(),
                        },
                    },
                };

                InstalledCrateInfo {
                    name: name.clone(),
                    cargo_version: cargo_version.clone(),
                    binstall_version,
                    method,
                    bins: bins.clone(),
                }
            })
            .collect()
    }

    pub fn prune_stale(mut self) -> Result<Vec<CompactString>, ManifestsError> {
        let mut pruned = Vec::new();
        let installed = &self.installed_crates;

        self.binstall.retain(|crate_info| {
            let is_valid = installed
                .get(&crate_info.name)
                .map(|ver| ver == &crate_info.current_version)
                .unwrap_or(false);

            if !is_valid {
                pruned.push(crate_info.name.clone());
            }
            is_valid
        });

        if !pruned.is_empty() {
            self.binstall.overwrite()?;
        }

        Ok(pruned)
    }

    pub fn update(mut self, metadata_vec: Vec<CrateInfo>) -> Result<(), ManifestsError> {
        self.rewind_cargo_crates_v1()?;

        CratesToml::append_to_file(&mut self.cargo_crates_v1, &metadata_vec)?;

        // Automatically prune uninstalled and version-drifted records
        let installed = &self.installed_crates;
        self.binstall.retain(|crate_info| {
            installed
                .get(&crate_info.name)
                .map(|ver| ver == &crate_info.current_version)
                .unwrap_or(false)
        });

        for metadata in metadata_vec {
            self.binstall.replace(metadata);
        }
        self.binstall.overwrite()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binstall_crates_v1::append_to_path;
    use binstalk_types::crate_info::{CrateInfo, CrateSource};
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_list_and_prune() {
        let tempdir = TempDir::new().unwrap();
        let cargo_roots = tempdir.path();

        // 1. Write mock .crates.toml
        let crates_toml_path = cargo_roots.join(".crates.toml");
        let toml_data = br#"
[v1]
"cargo-watch 8.5.3 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-watch"]
"sccache 0.18.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["sccache"]
"cargo-update 22.1.1 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-install-update"]
"my-local 0.1.0 (path+file:///some/local/path)" = ["my-local"]
"my-git 0.2.0 (git+https://github.com/user/repo)" = ["my-git"]
"#;
        File::create(&crates_toml_path)
            .unwrap()
            .write_all(toml_data)
            .unwrap();

        // 2. Write mock binstall crates-v1.json
        let binstall_dir = cargo_roots.join("binstall");
        fs::create_dir_all(&binstall_dir).unwrap();
        let binstall_json_path = binstall_dir.join("crates-v1.json");

        let binstall_data = vec![
            CrateInfo {
                name: "cargo-watch".into(),
                version_req: "8.5.3".into(),
                current_version: Version::new(8, 5, 3),
                source: CrateSource::cratesio_registry(),
                target: "x86_64-unknown-linux-gnu".into(),
                bins: vec!["cargo-watch".into()],
            },
            // sccache drifted: binstall has 0.17.0, cargo has 0.18.0
            CrateInfo {
                name: "sccache".into(),
                version_req: "0.17.0".into(),
                current_version: Version::new(0, 17, 0),
                source: CrateSource::cratesio_registry(),
                target: "x86_64-unknown-linux-gnu".into(),
                bins: vec!["sccache".into()],
            },
            // uninstalled-crate: in binstall manifest, but absent from .crates.toml
            CrateInfo {
                name: "uninstalled-crate".into(),
                version_req: "1.0.0".into(),
                current_version: Version::new(1, 0, 0),
                source: CrateSource::cratesio_registry(),
                target: "x86_64-unknown-linux-gnu".into(),
                bins: vec!["uninstalled-crate".into()],
            },
        ];
        append_to_path(&binstall_json_path, binstall_data).unwrap();

        // Test list_crates
        let manifests = Manifests::open_exclusive(cargo_roots).unwrap();
        let list = manifests.list_crates();
        assert_eq!(list.len(), 5);

        let find = |name: &str| list.iter().find(|c| c.name == name).unwrap();

        let watch = find("cargo-watch");
        assert_eq!(watch.method, InstalledMethod::Binstall);
        assert_eq!(watch.cargo_version, Version::new(8, 5, 3));
        assert_eq!(
            watch.binstall_version.as_ref(),
            Some(&Version::new(8, 5, 3))
        );

        let sccache = find("sccache");
        assert_eq!(
            sccache.method,
            InstalledMethod::Drifted {
                binstall_version: Version::new(0, 17, 0),
                cargo_version: Version::new(0, 18, 0),
            }
        );

        let update = find("cargo-update");
        assert_eq!(update.method, InstalledMethod::CargoInstall);
        assert_eq!(update.binstall_version, None);

        let local = find("my-local");
        assert_eq!(local.method, InstalledMethod::LocalPath);

        let git = find("my-git");
        assert_eq!(git.method, InstalledMethod::Git);

        // Test prune_stale
        let pruned = manifests.prune_stale().unwrap();
        assert_eq!(pruned.len(), 2);
        assert!(pruned.iter().any(|c| c == "sccache"));
        assert!(pruned.iter().any(|c| c == "uninstalled-crate"));

        // Verify state after pruning: sccache and uninstalled-crate gone from binstall
        let manifests_after = Manifests::open_exclusive(cargo_roots).unwrap();
        let list_after = manifests_after.list_crates();

        let sccache_after = list_after.iter().find(|c| c.name == "sccache").unwrap();
        assert_eq!(sccache_after.method, InstalledMethod::CargoInstall);
        assert_eq!(sccache_after.binstall_version, None);

        let watch_after = list_after.iter().find(|c| c.name == "cargo-watch").unwrap();
        assert_eq!(watch_after.method, InstalledMethod::Binstall);

        // Second prune should prune 0 crates
        let pruned_again = manifests_after.prune_stale().unwrap();
        assert!(pruned_again.is_empty());
    }

    #[test]
    fn test_update_auto_prunes_drift() {
        let tempdir = TempDir::new().unwrap();
        let cargo_roots = tempdir.path();

        let crates_toml_path = cargo_roots.join(".crates.toml");
        let toml_data = br#"
[v1]
"sccache 0.18.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["sccache"]
"#;
        File::create(&crates_toml_path)
            .unwrap()
            .write_all(toml_data)
            .unwrap();

        let binstall_dir = cargo_roots.join("binstall");
        fs::create_dir_all(&binstall_dir).unwrap();
        let binstall_json_path = binstall_dir.join("crates-v1.json");

        let binstall_data = vec![CrateInfo {
            name: "sccache".into(),
            version_req: "0.17.0".into(),
            current_version: Version::new(0, 17, 0),
            source: CrateSource::cratesio_registry(),
            target: "x86_64-unknown-linux-gnu".into(),
            bins: vec!["sccache".into()],
        }];
        append_to_path(&binstall_json_path, binstall_data).unwrap();

        let manifests = Manifests::open_exclusive(cargo_roots).unwrap();
        let new_crate = CrateInfo {
            name: "new-tool".into(),
            version_req: "1.0.0".into(),
            current_version: Version::new(1, 0, 0),
            source: CrateSource::cratesio_registry(),
            target: "x86_64-unknown-linux-gnu".into(),
            bins: vec!["new-tool".into()],
        };
        manifests.update(vec![new_crate]).unwrap();

        let records =
            crate::binstall_crates_v1::Records::load_from_path(&binstall_json_path).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records.get("new-tool").is_some());
        assert!(records.get("sccache").is_none());
    }
}
