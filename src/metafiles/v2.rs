use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Seek},
    iter::IntoIterator,
    path::{Path, PathBuf},
};

use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::CrateVersionSource;
use crate::{cargo_home, create_if_not_exist, FileLock};

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

    pub fn write_to_writer<W: io::Write>(&self, writer: W) -> Result<(), Crates2JsonParseError> {
        serde_json::to_writer(writer, &self)?;
        Ok(())
    }

    pub fn write_to_file(&self, file: &mut fs::File) -> Result<(), Crates2JsonParseError> {
        self.write_to_writer(&mut *file)?;
        let pos = file.stream_position()?;
        file.set_len(pos)?;

        Ok(())
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<(), Crates2JsonParseError> {
        let file = fs::File::create(path.as_ref())?;
        self.write_to_writer(file)
    }

    pub fn append_to_path<Iter>(
        path: impl AsRef<Path>,
        iter: Iter,
    ) -> Result<(), Crates2JsonParseError>
    where
        Iter: IntoIterator<Item = (CrateVersionSource, CrateInfo)>,
    {
        let mut file = FileLock::new_exclusive(create_if_not_exist(path.as_ref())?)?;
        let mut c2 = Self::load_from_reader(&mut *file)?;

        for (cvs, info) in iter {
            c2.insert(&cvs, info);
        }

        file.rewind()?;
        c2.write_to_file(&mut *file)?;

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
