use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    iter::IntoIterator,
    path::{Path, PathBuf},
    str::FromStr,
};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::CrateVersionSource;
use crate::cargo_home;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CratesToml {
    v1: BTreeMap<String, BTreeSet<String>>,
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
        Ok(toml::from_slice(&vec)?)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, CratesTomlParseError> {
        let file = fs::read_to_string(path)?;
        Self::from_str(&file)
    }

    pub fn insert(&mut self, cvs: &CrateVersionSource, bins: BTreeSet<String>) {
        self.v1.insert(cvs.to_string(), bins);
    }

    pub fn write(&self) -> Result<(), CratesTomlParseError> {
        self.write_to_path(Self::default_path()?)
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<(), CratesTomlParseError> {
        fs::write(path, &toml::to_vec(&self)?)?;
        Ok(())
    }

    pub fn append_to_path<'a, Iter>(
        path: impl AsRef<Path>,
        iter: Iter,
    ) -> Result<(), CratesTomlParseError>
    where
        Iter: IntoIterator<Item = (&'a CrateVersionSource, BTreeSet<String>)>,
    {
        let mut c1 = match Self::load_from_path(path.as_ref()) {
            Ok(c1) => c1,
            Err(CratesTomlParseError::Io(io_err)) if io_err.kind() == io::ErrorKind::NotFound => {
                Self::default()
            }
            Err(err) => return Err(err),
        };
        for (cvs, bins) in iter {
            c1.insert(cvs, bins);
        }
        c1.write_to_path(path.as_ref())?;

        Ok(())
    }

    pub fn append<'a, Iter>(iter: Iter) -> Result<(), CratesTomlParseError>
    where
        Iter: IntoIterator<Item = (&'a CrateVersionSource, BTreeSet<String>)>,
    {
        Self::append_to_path(Self::default_path()?, iter)
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
    Io(#[from] io::Error),

    #[error(transparent)]
    TomlParse(#[from] toml::de::Error),

    #[error(transparent)]
    TomlWrite(#[from] toml::ser::Error),

    #[error(transparent)]
    CvsParse(#[from] super::CvsParseError),
}
