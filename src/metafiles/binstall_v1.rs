use std::{
    borrow, cmp,
    collections::{btree_set, BTreeSet},
    fs, hash,
    io::{self, Write},
    iter::{IntoIterator, Iterator},
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

impl borrow::Borrow<str> for MetaData {
    fn borrow(&self) -> &str {
        &self.name
    }
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

    write_to(file, &mut iter.into_iter())
}

pub fn write_to(file: FileLock, iter: &mut dyn Iterator<Item = MetaData>) -> Result<(), Error> {
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
    /// Use BTreeSet to dedup the metadata
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

    /// **Warning: This will overwrite all existing records!**
    pub fn overwrite(self) -> Result<(), Error> {
        write_to(self.file, &mut self.data.into_iter())
    }

    pub fn get(&self, value: impl AsRef<str>) -> Option<&MetaData> {
        self.data.get(value.as_ref())
    }

    pub fn contains(&self, value: impl AsRef<str>) -> bool {
        self.data.contains(value.as_ref())
    }

    /// Adds a value to the set.
    /// If the set did not have an equal element present, true is returned.
    ///
    /// If the set did have an equal element present, false is returned, and the entry is not updated. See the module-level documentation for more.
    pub fn insert(&mut self, value: MetaData) -> bool {
        self.data.insert(value)
    }

    pub fn replace(&mut self, value: MetaData) -> Option<MetaData> {
        self.data.replace(value)
    }

    pub fn remove(&mut self, value: impl AsRef<str>) -> bool {
        self.data.remove(value.as_ref())
    }

    pub fn take(&mut self, value: impl AsRef<str>) -> Option<MetaData> {
        self.data.take(value.as_ref())
    }
}

impl<'a> IntoIterator for &'a Records {
    type Item = &'a MetaData;

    type IntoIter = btree_set::Iter<'a, MetaData>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}
