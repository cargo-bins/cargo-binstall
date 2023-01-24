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
    str::{self, Utf8Error},
};

use beef::Cow;
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
pub struct CratesToml<'a> {
    #[serde(with = "tuple_vec_map")]
    v1: Vec<(String, Cow<'a, [CompactString]>)>,
}

impl CratesToml<'_> {
    pub fn default_path() -> Result<PathBuf, CratesTomlParseError> {
        Ok(cargo_home()?.join(".crates.toml"))
    }

    pub fn load() -> Result<Self, CratesTomlParseError> {
        Self::load_from_path(Self::default_path()?)
    }

    pub fn load_from_reader<R: io::Read>(mut reader: R) -> Result<Self, CratesTomlParseError> {
        fn inner(reader: &mut dyn io::Read) -> Result<CratesToml<'static>, CratesTomlParseError> {
            let mut vec = Vec::new();
            reader.read_to_end(&mut vec)?;

            if vec.is_empty() {
                Ok(CratesToml::default())
            } else {
                toml::from_str(str::from_utf8(&vec)?).map_err(CratesTomlParseError::from)
            }
        }

        inner(&mut reader)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, CratesTomlParseError> {
        let file = FileLock::new_shared(File::open(path)?)?;
        Self::load_from_reader(file)
    }

    /// Only use it when you know that the crate is not in the manifest.
    /// Otherwise, you need to call [`CratesToml::remove`] first.
    pub fn insert(&mut self, cvs: &CrateVersionSource, bins: Vec<CompactString>) {
        self.v1.push((cvs.to_string(), Cow::owned(bins)));
    }

    pub fn remove(&mut self, name: &str) {
        self.v1.retain(|(s, _bin)| {
            s.split_once(' ')
                .map(|(crate_name, _rest)| crate_name != name)
                .unwrap_or_default()
        });
    }

    pub fn write(&self) -> Result<(), CratesTomlParseError> {
        self.write_to_path(Self::default_path()?)
    }

    pub fn write_to_writer<W: io::Write>(&self, mut writer: W) -> Result<(), CratesTomlParseError> {
        fn inner(
            this: &CratesToml<'_>,
            writer: &mut dyn io::Write,
        ) -> Result<(), CratesTomlParseError> {
            let data = toml::to_string(&this)?.into_bytes();
            writer.write_all(&data)?;
            Ok(())
        }

        inner(self, &mut writer)
    }

    pub fn write_to_file(&self, file: &mut File) -> Result<(), CratesTomlParseError> {
        self.write_to_writer(&mut *file)?;
        let pos = file.stream_position()?;
        file.set_len(pos)?;

        Ok(())
    }

    pub fn write_to_path(&self, path: impl AsRef<Path>) -> Result<(), CratesTomlParseError> {
        let mut file = FileLock::new_exclusive(File::create(path)?)?;
        self.write_to_file(&mut file)
    }

    pub fn append_to_file<'a, Iter>(file: &mut File, iter: Iter) -> Result<(), CratesTomlParseError>
    where
        Iter: IntoIterator<Item = &'a CrateInfo>,
    {
        fn inner(
            file: &mut File,
            iter: &mut dyn Iterator<Item = &CrateInfo>,
        ) -> Result<(), CratesTomlParseError> {
            let mut c1 = CratesToml::load_from_reader(&mut *file)?;

            for metadata in iter {
                let name = &metadata.name;
                let version = &metadata.current_version;
                let source = Source::from(&metadata.source);

                c1.remove(name);
                c1.v1.push((
                    format!("{name} {version} ({source})"),
                    Cow::borrowed(&metadata.bins),
                ));
            }

            file.rewind()?;
            c1.write_to_file(file)?;

            Ok(())
        }

        inner(file, &mut iter.into_iter())
    }

    pub fn append_to_path<'a, Iter>(
        path: impl AsRef<Path>,
        iter: Iter,
    ) -> Result<(), CratesTomlParseError>
    where
        Iter: IntoIterator<Item = &'a CrateInfo>,
    {
        let mut file = FileLock::new_exclusive(create_if_not_exist(path.as_ref())?)?;
        Self::append_to_file(&mut file, iter)
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
        fn parse_name_ver(s: &str) -> Result<(CompactString, Version), CvsParseError> {
            match s.splitn(3, ' ').collect::<Vec<_>>()[..] {
                [name, version, _source] => Ok((CompactString::new(name), version.parse()?)),
                _ => Err(CvsParseError::BadFormat),
            }
        }

        self.v1
            .into_iter()
            .map(|(s, _bins)| parse_name_ver(&s).map_err(CratesTomlParseError::from))
            .collect()
    }
}

#[derive(Debug, Diagnostic, Error)]
#[non_exhaustive]
pub enum CratesTomlParseError {
    #[error(transparent)]
    Io(#[from] io::Error),

    #[error("Failed to parse toml: File is not in valid utf-8 encodings: {0}")]
    TomlParseNonUtf8(#[from] Utf8Error),

    #[error(transparent)]
    TomlParse(Box<toml::de::Error>),

    #[error(transparent)]
    TomlWrite(Box<toml::ser::Error>),

    #[error(transparent)]
    CvsParse(Box<CvsParseError>),
}

impl From<CvsParseError> for CratesTomlParseError {
    fn from(e: CvsParseError) -> Self {
        CratesTomlParseError::CvsParse(Box::new(e))
    }
}

impl From<toml::ser::Error> for CratesTomlParseError {
    fn from(e: toml::ser::Error) -> Self {
        CratesTomlParseError::TomlWrite(Box::new(e))
    }
}

impl From<toml::de::Error> for CratesTomlParseError {
    fn from(e: toml::de::Error) -> Self {
        CratesTomlParseError::TomlParse(Box::new(e))
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

        // Update
        CratesToml::append_to_path(
            &path,
            &[CrateInfo {
                name: "cargo-binstall".into(),
                version_req: "*".into(),
                current_version: Version::new(0, 12, 0),
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
            &Version::new(0, 12, 0)
        );
    }

    #[test]
    fn test_empty_file() {
        let tempdir = TempDir::new().unwrap();
        let path = tempdir.path().join("crates-v1.toml");

        File::create(&path).unwrap();

        assert!(CratesToml::load_from_path(&path).unwrap().v1.is_empty());
    }

    #[test]
    fn test_loading() {
        let raw_data = br#"
[v1]
"alacritty 0.10.1 (registry+https://github.com/rust-lang/crates.io-index)" = ["alacritty"]
"cargo-audit 0.17.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-audit"]
"cargo-binstall 0.10.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-binstall"]
"cargo-criterion 1.1.0 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-criterion"]
"cargo-edit 0.10.1 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-add", "cargo-rm", "cargo-set-version", "cargo-upgrade"]
"cargo-expand 1.0.27 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-expand"]
"cargo-geiger 0.11.3 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-geiger"]
"cargo-hack 0.5.15 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-hack"]
"cargo-nextest 0.9.26 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-nextest"]
"cargo-supply-chain 0.3.1 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-supply-chain"]
"cargo-tarpaulin 0.20.1 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-tarpaulin"]
"cargo-update 8.1.4 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-install-update", "cargo-install-update-config"]
"cargo-watch 8.1.2 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-watch"]
"cargo-with 0.3.2 (registry+https://github.com/rust-lang/crates.io-index)" = ["cargo-with"]
"cross 0.2.4 (registry+https://github.com/rust-lang/crates.io-index)" = ["cross", "cross-util"]
"irust 1.63.3 (registry+https://github.com/rust-lang/crates.io-index)" = ["irust"]
"tokei 12.1.2 (registry+https://github.com/rust-lang/crates.io-index)" = ["tokei"]
"xargo 0.3.26 (registry+https://github.com/rust-lang/crates.io-index)" = ["xargo", "xargo-check"]
        "#;

        CratesToml::load_from_reader(raw_data.as_slice()).unwrap();
    }
}
