use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Seek},
    iter::IntoIterator,
    path::{Path, PathBuf},
};

use compact_str::CompactString;
use miette::Diagnostic;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{binstall_v1::MetaData, CrateVersionSource};
use crate::{cargo_home, create_if_not_exist, FileLock};

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
        let file = File::open(path)?;
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
        Iter: IntoIterator<Item = &'a MetaData>,
    {
        let mut file = FileLock::new_exclusive(create_if_not_exist(path.as_ref())?)?;
        let mut c1 = Self::load_from_reader(&mut *file)?;

        for metadata in iter {
            c1.insert(&CrateVersionSource::from(metadata), metadata.bins.clone());
        }

        file.rewind()?;
        c1.write_to_file(&mut *file)?;

        Ok(())
    }

    pub fn append<'a, Iter>(iter: Iter) -> Result<(), CratesTomlParseError>
    where
        Iter: IntoIterator<Item = &'a MetaData>,
    {
        Self::append_to_path(Self::default_path()?, iter)
    }
}

#[derive(Debug, Diagnostic, Error)]
pub enum CratesTomlParseError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    TomlParse(#[from] toml_edit::easy::de::Error),

    #[error(transparent)]
    TomlWrite(#[from] toml_edit::easy::ser::Error),

    #[error(transparent)]
    CvsParse(#[from] super::CvsParseError),
}
