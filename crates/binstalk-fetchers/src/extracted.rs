use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use binstalk_downloader::download::ExtractedFiles;
use compact_str::CompactString;

/// A package that [`Found::fetch_and_extract`](crate::Found::fetch_and_extract)
/// extracted, maybe for other crates in the same package too.
#[derive(Debug)]
pub struct Extracted {
    crate_name: CompactString,
    dir: PathBuf,
    files: ExtractedFiles,
    installs_left: InstallsLeft,
}

impl Extracted {
    /// `files`, extracted to `dir` for the crate `crate_name` only.
    pub fn new(crate_name: CompactString, dir: PathBuf, files: ExtractedFiles) -> Self {
        Self {
            crate_name,
            dir,
            files,
            installs_left: InstallsLeft(AtomicUsize::new(1)),
        }
    }

    /// The crate it was extracted for first.
    pub fn crate_name(&self) -> &str {
        &self.crate_name
    }

    /// Where it was extracted to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn files(&self) -> &ExtractedFiles {
        &self.files
    }

    /// Give it to one more crate.
    pub(crate) fn add_crate(&self) {
        self.installs_left.add();
    }

    /// Called once by every crate it was given to, when installing from it.
    ///
    /// `true` for the last one, which may move files out of [`Self::dir`];
    /// the others must copy them, as another crate may install them too.
    /// That's only known once every crate it may be given to is resolved,
    /// so none of them must install before then.
    pub fn may_move_files(&self) -> bool {
        self.installs_left.install()
    }
}

/// How many crates it was given to haven't installed from it yet.
#[derive(Debug)]
struct InstallsLeft(AtomicUsize);

impl InstallsLeft {
    fn add(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether it was the last one.
    fn install(&self) -> bool {
        self.0.fetch_sub(1, Ordering::AcqRel) == 1
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_last_install_may_move_files() {
        let installs_left = InstallsLeft(AtomicUsize::new(1));
        installs_left.add();
        installs_left.add();
        assert!(!installs_left.install());
        assert!(!installs_left.install());
        assert!(installs_left.install());
    }
}
