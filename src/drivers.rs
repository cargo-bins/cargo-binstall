use std::path::{Path, PathBuf};

use crate::BinstallError;

mod vfs;

mod visitor;
use visitor::ManifestVisitor;

mod version;
use version::find_version;

mod crates_io;
pub use crates_io::fetch_crate_cratesio;

/// Fetch a crate by name and version from github
/// TODO: implement this
pub async fn fetch_crate_gh_releases(
    _name: &str,
    _version: Option<&str>,
    _temp_dir: &Path,
) -> Result<PathBuf, BinstallError> {
    unimplemented!();
}
