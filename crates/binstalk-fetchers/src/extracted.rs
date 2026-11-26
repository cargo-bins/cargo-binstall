use std::path::{Path, PathBuf};

use binstalk_downloader::download::ExtractedFiles;

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
