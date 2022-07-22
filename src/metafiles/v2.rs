use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    iter::IntoIterator,
    path::{Path, PathBuf},
};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::CrateVersionSource;
use crate::cargo_home;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Crates2Json {
    pub installs: BTreeMap<String, CrateInfo>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CrateInfo {
    #[serde(default)]
    pub version_req: Option<String>,

    #[serde(default)]
    pub bins: BTreeSet<String>,

    #[serde(default)]
    pub features: BTreeSet<String>,

    #[serde(default)]
    pub all_features: bool,

    #[serde(default)]
    pub no_default_features: bool,

    pub profile: String,
    pub target: String,
    pub rustc: String,
}

impl Crates2Json {
    pub fn default_path() -> Result<PathBuf, Crates2JsonParseError> {
        Ok(cargo_home()?.join(".crates2.json"))
    }

    pub fn load() -> Result<Self, Crates2JsonParseError> {
        Self::load_from_path(Self::default_path()?)
    }

    pub fn load_from_reader<R: io::Read>(reader: R) -> Result<Self, Crates2JsonParseError> {
        Ok(serde_json::from_reader(reader)?)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, Crates2JsonParseError> {
        let file = fs::File::open(path.as_ref())?;
        Self::load_from_reader(file)
    }

    pub fn insert(&mut self, cvs: &CrateVersionSource, info: CrateInfo) {
        self.installs.insert(cvs.to_string(), info);
    }

    pub fn write(&self) -> Result<(), Crates2JsonParseError> {
        self.write_to_path(Self::default_path()?)
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<(), Crates2JsonParseError> {
        let file = fs::File::create(path.as_ref())?;
        serde_json::to_writer(file, &self)?;
        Ok(())
    }

    pub fn append_to_path<Iter>(
        path: impl AsRef<Path>,
        iter: Iter,
    ) -> Result<(), Crates2JsonParseError>
    where
        Iter: IntoIterator<Item = (CrateVersionSource, CrateInfo)>,
    {
        let mut c2 = match Self::load_from_path(path.as_ref()) {
            Ok(c2) => c2,
            Err(Crates2JsonParseError::Io(io_err)) if io_err.kind() == io::ErrorKind::NotFound => {
                Self::default()
            }
            Err(err) => return Err(err),
        };
        for (cvs, info) in iter {
            c2.insert(&cvs, info);
        }
        c2.write_to_path(path.as_ref())?;

        Ok(())
    }

    pub fn append<Iter>(iter: Iter) -> Result<(), Crates2JsonParseError>
    where
        Iter: IntoIterator<Item = (CrateVersionSource, CrateInfo)>,
    {
        Self::append_to_path(Self::default_path()?, iter)
    }
}

#[derive(Debug, Diagnostic, Error)]
pub enum Crates2JsonParseError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    CvsParse(#[from] super::CvsParseError),
}
