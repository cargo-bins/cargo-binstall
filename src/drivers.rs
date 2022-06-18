use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use log::debug;
use semver::{Version, VersionReq};

use crate::BinstallError;

mod cratesio;
pub use cratesio::*;

mod vfs;

mod visitor;

fn find_version<'a, V: Iterator<Item = &'a String>>(
    requirement: &str,
    version_iter: V,
) -> Result<Version, BinstallError> {
    // Parse version requirement
    let version_req = VersionReq::parse(requirement).map_err(|err| BinstallError::VersionReq {
        req: requirement.into(),
        err,
    })?;

    // Filter for matching versions
    let filtered: BTreeSet<_> = version_iter
        .filter_map(|v| {
            // Remove leading `v` for git tags
            let ver_str = match v.strip_prefix('s') {
                Some(v) => v,
                None => v,
            };

            // Parse out version
            let ver = Version::parse(ver_str).ok()?;
            debug!("Version: {:?}", ver);

            // Filter by version match
            if version_req.matches(&ver) {
                Some(ver)
            } else {
                None
            }
        })
        .collect();

    debug!("Filtered: {:?}", filtered);

    // Return highest version
    filtered
        .iter()
        .max()
        .cloned()
        .ok_or(BinstallError::VersionMismatch { req: version_req })
}

/// Fetch a crate by name and version from github
/// TODO: implement this
pub async fn fetch_crate_gh_releases(
    _name: &str,
    _version: Option<&str>,
    _temp_dir: &Path,
) -> Result<PathBuf, BinstallError> {
    unimplemented!();
}
