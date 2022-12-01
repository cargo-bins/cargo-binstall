//! Cargo's `.crates.toml` manifest.
//!
//! This manifest is used by Cargo to record which crates were installed by `cargo-install` and by
//! other Cargo (first and third party) tooling to act upon these crates (e.g. upgrade them, list
//! them, etc).
//!
//! Binstall writes to this manifest when installing a crate, for interoperability with the Cargo
//! ecosystem.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Seek},
    iter::IntoIterator,
    path::{Path, PathBuf},
    str::FromStr,
};

use compact_str::CompactString;
use fs_lock::FileLock;
use home::cargo_home;
use miette::Diagnostic;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::helpers::create_if_not_exist;

use super::crate_info::CrateInfo;

mod crate_version_source;
use crate_version_source::*;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CratesToml {
    v1: BTreeMap<String, Vec<CompactString>>,
}

impl CratesToml {
    pub fn default_path() -> Result<PathBuf, CratesTomlParseError> {
        Ok(cargo_home()?.join(".crates.toml"))
    }

    pub fn load() -> Result<Self, CratesTomlParseError> {
        Self::load_from_path(Self::default_path()?)
    }

    pub fn load_from_reader<R: io::Read>(mut reader: R) -> Result<Self, CratesTomlParseError> {
        let mut vec = Vec::new();
        reader.read_to_end(&mut vec)?;
        Ok(toml_edit::easy::from_slice(&vec)?)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, CratesTomlParseError> {
        let file = FileLock::new_shared(File::open(path)?)?;
        Self::load_from_reader(file)
    }

    pub fn insert(&mut self, cvs: &CrateVersionSource, bins: Vec<CompactString>) {
        self.v1.insert(cvs.to_string(), bins);
    }

    pub fn write(&self) -> Result<(), CratesTomlParseError> {
        self.write_to_path(Self::default_path()?)
    }

    pub fn write_to_writer<W: io::Write>(&self, mut writer: W) -> Result<(), CratesTomlParseError> {
        let data = toml_edit::easy::to_vec(&self)?;
        writer.write_all(&data)?;
        Ok(())
    }

    pub fn write_to_file(&self, file: &mut File) -> Result<(), CratesTomlParseError> {
        self.write_to_writer(&mut *file)?;
        let pos = file.stream_position()?;
        file.set_len(pos)?;

        Ok(())
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<(), CratesTomlParseError> {
        let mut file = File::create(path)?;
        self.write_to_file(&mut file)
    }

    pub fn append_to_path<'a, Iter>(
        path: impl AsRef<Path>,
        iter: Iter,
    ) -> Result<(), CratesTomlParseError>
    where
        Iter: IntoIterator<Item = &'a CrateInfo>,
    {
        let mut file = FileLock::new_exclusive(create_if_not_exist(path.as_ref())?)?;
        let mut c1 = if file.metadata()?.len() != 0 {
            Self::load_from_reader(&mut *file)?
        } else {
            Self::default()
        };

        for metadata in iter {
            c1.insert(&CrateVersionSource::from(metadata), metadata.bins.clone());
        }

        file.rewind()?;
        c1.write_to_file(&mut file)?;

        Ok(())
    }

    pub fn append<'a, Iter>(iter: Iter) -> Result<(), CratesTomlParseError>
    where
        Iter: IntoIterator<Item = &'a CrateInfo>,
    {
        Self::append_to_path(Self::default_path()?, iter)
    }

    /// Return BTreeMap with crate name as key and its corresponding version
    /// as value.
    pub fn collect_into_crates_versions(
        self,
    ) -> Result<BTreeMap<CompactString, Version>, CratesTomlParseError> {
        self.v1
            .into_keys()
            .map(|s| {
                let cvs = CrateVersionSource::from_str(&s)?;
                Ok((cvs.name, cvs.version))
            })
            .collect()
    }
}

#[derive(Debug, Diagnostic, Error)]
#[non_exhaustive]
pub enum CratesTomlParseError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    TomlParse(#[from] toml_edit::easy::de::Error),

    #[error(transparent)]
    TomlWrite(Box<toml_edit::easy::ser::Error>),

    #[error(transparent)]
    CvsParse(Box<CvsParseError>),
}

impl From<CvsParseError> for CratesTomlParseError {
    fn from(e: CvsParseError) -> Self {
        CratesTomlParseError::CvsParse(Box::new(e))
    }
}

impl From<toml_edit::easy::ser::Error> for CratesTomlParseError {
    fn from(e: toml_edit::easy::ser::Error) -> Self {
        CratesTomlParseError::TomlWrite(Box::new(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crate_info::CrateSource;

    use detect_targets::TARGET;
    use semver::Version;
    use tempfile::TempDir;

    #[test]
    fn test_empty() {
        let tempdir = TempDir::new().unwrap();
        let path = tempdir.path().join("crates-v1.toml");

        CratesToml::append_to_path(
            &path,
            &[CrateInfo {
                name: "cargo-binstall".into(),
                version_req: "*".into(),
                current_version: Version::new(0, 11, 1),
                source: CrateSource::cratesio_registry(),
                target: TARGET.into(),
                bins: vec!["cargo-binstall".into()],
            }],
        )
        .unwrap();

        let crates = CratesToml::load_from_path(&path)
            .unwrap()
            .collect_into_crates_versions()
            .unwrap();

        assert_eq!(crates.len(), 1);

        assert_eq!(
            crates.get("cargo-binstall").unwrap(),
            &Version::new(0, 11, 1)
        );
    }
}
