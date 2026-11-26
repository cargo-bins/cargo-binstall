use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use binstalk_downloader::download::ExtractedFiles;
use binstalk_types::cargo_toml_binstall::PkgFmt;
use compact_str::CompactString;

/// A package that [`Found::fetch_and_extract`](crate::Found::fetch_and_extract)
/// extracted, maybe for other crates in the same package too.
#[derive(Debug)]
pub struct Extracted {
    dir: PathBuf,
    files: ExtractedFiles,
}

impl Extracted {
    /// `files`, extracted to `dir`.
    pub fn new(dir: PathBuf, files: ExtractedFiles) -> Self {
        Self { dir, files }
    }

    /// Where it was extracted to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn files(&self) -> &ExtractedFiles {
        &self.files
    }
}

/// What [`Found::fetch_and_extract`](crate::Found::fetch_and_extract)
/// fetched.
#[derive(Debug)]
pub enum Fetched {
    /// One package providing all the binaries.
    Package(Arc<Extracted>),
    /// One package per binary, keyed by binary name, when `pkg-url` contains
    /// `{ bin }`.
    ///
    /// Optional binaries whose package was not found are left out, see
    /// [`Data::with_bins`](crate::Data::with_bins).
    PerBin(BTreeMap<CompactString, BinPackage>),
}

/// The package of one binary, see [`Fetched::PerBin`].
#[derive(Debug)]
pub struct BinPackage {
    pub pkg_fmt: PkgFmt,
    pub extracted: Arc<Extracted>,
}
