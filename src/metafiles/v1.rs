use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::CrateVersionSource;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CratesToml {
    v1: BTreeMap<CrateVersionSource, BTreeSet<String>>,
}

impl CratesToml {
    pub fn default_path() -> Result<PathBuf, CratesTomlParseError> {
        Ok(home::cargo_home()?.join(".crates.toml"))
    }

    pub fn load() -> Result<Self, CratesTomlParseError> {
        Self::load_from_path(Self::default_path()?)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, CratesTomlParseError> {
        let file = fs::read_to_string(path)?;
        Self::from_str(&file)
    }

    pub fn insert(&mut self, cvs: CrateVersionSource, bins: BTreeSet<String>) {
        self.v1.insert(cvs, bins);
    }

    pub fn write(&self) -> Result<(), CratesTomlParseError> {
        self.write_to_path(Self::default_path()?)
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<(), CratesTomlParseError> {
        fs::write(path, &toml::to_vec(&self)?)?;
        Ok(())
    }
}

impl FromStr for CratesToml {
    type Err = CratesTomlParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(toml::from_str(s)?)
    }
}

#[derive(Debug, Diagnostic, Error)]
pub enum CratesTomlParseError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    TomlParse(#[from] toml::de::Error),

    #[error(transparent)]
    TomlWrite(#[from] toml::ser::Error),

    #[error(transparent)]
    CvsParse(#[from] super::CvsParseError),
}
