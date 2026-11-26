use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use binstalk_downloader::download::ExtractedFiles;
use binstalk_types::cargo_toml_binstall::PkgMeta;
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

/// A package that [`Found::fetch_and_extract`](crate::Found::fetch_and_extract)
/// fetched.
#[derive(Debug)]
pub struct Fetched {
    /// The binary it is the package of, when each binary is in its own
    /// package because `pkg-url` contains `{ bin }`. `None` if it provides
    /// all of them.
    ///
    /// Optional binaries whose package was not found have none, see
    /// [`Data::with_bins`](crate::Data::with_bins).
    pub bin: Option<CompactString>,
    /// The metadata to find the binaries in it with, see
    /// [`Found::target_meta`](crate::Found::target_meta).
    pub meta: PkgMeta,
    pub extracted: Arc<Extracted>,
}
