use std::{
    cmp,
    collections::BTreeSet,
    fs, hash,
    io::{self, Write},
    iter::IntoIterator,
    path::{Path, PathBuf},
};

use compact_str::CompactString;
use miette::Diagnostic;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::{cargo_home, cratesio_url, create_if_not_exist, FileLock};

#[derive(Debug, Serialize, Deserialize)]
pub struct MetaData {
    pub name: CompactString,
    pub version_req: CompactString,
    pub current_version: Version,
    pub source: Source,
    pub target: CompactString,
    pub bins: Vec<CompactString>,
}
impl PartialEq for MetaData {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for MetaData {}

impl PartialOrd for MetaData {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        self.name.partial_cmp(&other.name)
    }
}

impl Ord for MetaData {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl hash::Hash for MetaData {
    fn hash<H>(&self, state: &mut H)
    where
        H: hash::Hasher,
    {
        self.name.hash(state)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub enum SourceType {
    Git,
    Path,
    Registry,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Source {
    pub source_type: SourceType,
    pub url: Url,
}

impl Source {
    pub fn cratesio_registry() -> Source {
        Self {
            source_type: SourceType::Registry,
            url: cratesio_url().clone(),
        }
    }
}

#[derive(Debug, Diagnostic, Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    SerdeJsonParse(#[from] serde_json::Error),
}

pub fn append_to_path<Iter>(path: impl AsRef<Path>, iter: Iter) -> Result<(), Error>
where
    Iter: IntoIterator<Item = MetaData>,
{
    let file = FileLock::new_exclusive(
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?,
    )?;

    let writer = io::BufWriter::with_capacity(512, file);

    let mut ser = serde_json::Serializer::new(writer);

    for item in iter {
        item.serialize(&mut ser)?;
    }

    ser.into_inner().flush()?;

    Ok(())
}

pub fn default_path() -> Result<PathBuf, Error> {
    Ok(cargo_home()?.join(".binstall-crates.toml"))
}

#[derive(Debug)]
pub struct Records {
    file: FileLock,
    data: BTreeSet<MetaData>,
}

impl Records {
    fn load_impl(&mut self) -> Result<(), Error> {
        let reader = io::BufReader::with_capacity(1024, &mut self.file);
        let stream_deser = serde_json::Deserializer::from_reader(reader).into_iter();

        for res in stream_deser {
            let item = res?;

            self.data.replace(item);
        }

        Ok(())
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, Error> {
        let mut this = Self {
            file: FileLock::new_exclusive(create_if_not_exist(path.as_ref())?)?,
            data: BTreeSet::default(),
        };
        this.load_impl()?;
        Ok(this)
    }

    pub fn load() -> Result<Self, Error> {
        Self::load_from_path(default_path()?)
    }
}
