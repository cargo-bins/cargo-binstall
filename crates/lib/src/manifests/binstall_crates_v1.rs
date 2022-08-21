//! Binstall's `crates-v1.json` manifest.
//!
//! This manifest is used by Binstall to record which crates were installed, and may be used by
//! other (third party) tooling to act upon these crates (e.g. upgrade them, list them, etc).
//!
//! The format is a series of JSON object concatenated together. It is _not_ NLJSON, though writing
//! NLJSON to the file will be understood fine.

use std::{
    collections::{btree_set, BTreeSet},
    fs,
    io::{self, Seek, Write},
    iter::{IntoIterator, Iterator},
    path::{Path, PathBuf},
};

use flock::FileLock;
use miette::Diagnostic;
use serde::Serialize;
use thiserror::Error;

use crate::{fs::create_if_not_exist, helpers::statics::cargo_home};

use super::crate_info::CrateInfo;

#[derive(Debug, Diagnostic, Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    SerdeJsonParse(#[from] serde_json::Error),
}

pub fn append_to_path<Iter>(path: impl AsRef<Path>, iter: Iter) -> Result<(), Error>
where
    Iter: IntoIterator<Item = CrateInfo>,
{
    let mut file = FileLock::new_exclusive(create_if_not_exist(path.as_ref())?)?;
    // Move the cursor to EOF
    file.seek(io::SeekFrom::End(0))?;

    write_to(&mut file, &mut iter.into_iter())
}

pub fn append<Iter>(iter: Iter) -> Result<(), Error>
where
    Iter: IntoIterator<Item = CrateInfo>,
{
    append_to_path(default_path()?, iter)
}

pub fn write_to(
    file: &mut FileLock,
    iter: &mut dyn Iterator<Item = CrateInfo>,
) -> Result<(), Error> {
    let writer = io::BufWriter::with_capacity(512, file);

    let mut ser = serde_json::Serializer::new(writer);

    for item in iter {
        item.serialize(&mut ser)?;
    }

    ser.into_inner().flush()?;

    Ok(())
}

pub fn default_path() -> Result<PathBuf, Error> {
    let dir = cargo_home()?.join("binstall");

    fs::create_dir_all(&dir)?;

    Ok(dir.join("crates-v1.json"))
}

#[derive(Debug)]
pub struct Records {
    file: FileLock,
    /// Use BTreeSet to dedup the metadata
    data: BTreeSet<CrateInfo>,
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
    pub fn overwrite(mut self) -> Result<(), Error> {
        self.file.rewind()?;
        write_to(&mut self.file, &mut self.data.into_iter())?;

        let len = self.file.stream_position()?;
        self.file.set_len(len)?;

        Ok(())
    }

    pub fn get(&self, value: impl AsRef<str>) -> Option<&CrateInfo> {
        self.data.get(value.as_ref())
    }

    pub fn contains(&self, value: impl AsRef<str>) -> bool {
        self.data.contains(value.as_ref())
    }

    /// Adds a value to the set.
    /// If the set did not have an equal element present, true is returned.
    /// If the set did have an equal element present, false is returned,
    /// and the entry is not updated.
    pub fn insert(&mut self, value: CrateInfo) -> bool {
        self.data.insert(value)
    }

    pub fn replace(&mut self, value: CrateInfo) -> Option<CrateInfo> {
        self.data.replace(value)
    }

    pub fn remove(&mut self, value: impl AsRef<str>) -> bool {
        self.data.remove(value.as_ref())
    }

    pub fn take(&mut self, value: impl AsRef<str>) -> Option<CrateInfo> {
        self.data.take(value.as_ref())
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl<'a> IntoIterator for &'a Records {
    type Item = &'a CrateInfo;

    type IntoIter = btree_set::Iter<'a, CrateInfo>;

    fn into_iter(self) -> Self::IntoIter {
        self.data.iter()
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::manifests::crate_info::CrateSource;

    use compact_str::CompactString;
    use detect_targets::TARGET;
    use semver::Version;
    use tempfile::NamedTempFile;

    macro_rules! assert_records_eq {
        ($records:expr, $metadata_set:expr) => {
            assert_eq!($records.len(), $metadata_set.len());
            for (record, metadata) in $records.into_iter().zip($metadata_set.iter()) {
                assert_eq!(record, metadata);
            }
        };
    }

    #[test]
    fn rw_test() {
        let target = CompactString::from(TARGET);

        let named_tempfile = NamedTempFile::new().unwrap();
        let path = named_tempfile.path();

        let metadata_vec = [
            CrateInfo {
                name: "a".into(),
                version_req: "*".into(),
                current_version: Version::new(0, 1, 0),
                source: CrateSource::cratesio_registry(),
                target: target.clone(),
                bins: vec!["1".into(), "2".into()],
                other: Default::default(),
            },
            CrateInfo {
                name: "b".into(),
                version_req: "0.1.0".into(),
                current_version: Version::new(0, 1, 0),
                source: CrateSource::cratesio_registry(),
                target: target.clone(),
                bins: vec!["1".into(), "2".into()],
                other: Default::default(),
            },
            CrateInfo {
                name: "a".into(),
                version_req: "*".into(),
                current_version: Version::new(0, 2, 0),
                source: CrateSource::cratesio_registry(),
                target: target.clone(),
                bins: vec!["1".into()],
                other: Default::default(),
            },
        ];

        append_to_path(&path, metadata_vec.clone()).unwrap();

        let mut iter = metadata_vec.into_iter();
        iter.next().unwrap();

        let mut metadata_set: BTreeSet<_> = iter.collect();

        let mut records = Records::load_from_path(&path).unwrap();
        assert_records_eq!(&records, &metadata_set);

        records.remove("b");
        assert_eq!(records.len(), metadata_set.len() - 1);
        records.overwrite().unwrap();

        metadata_set.remove("b");
        let records = Records::load_from_path(&path).unwrap();
        assert_records_eq!(&records, &metadata_set);
        // Drop the exclusive file lock
        drop(records);

        let new_metadata = CrateInfo {
            name: "b".into(),
            version_req: "0.1.0".into(),
            current_version: Version::new(0, 1, 1),
            source: CrateSource::cratesio_registry(),
            target,
            bins: vec!["1".into(), "2".into()],
            other: Default::default(),
        };
        append_to_path(&path, [new_metadata.clone()]).unwrap();
        metadata_set.insert(new_metadata);

        let records = Records::load_from_path(&path).unwrap();
        assert_records_eq!(&records, &metadata_set);
    }
}
