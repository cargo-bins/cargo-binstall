use log::debug;
use semver::VersionReq;

use crate::BinstallError;

pub(super) trait Version {
    /// Return `None` on error.
    fn get_version(&self) -> Option<semver::Version>;
}

impl<T: Version> Version for &T {
    fn get_version(&self) -> Option<semver::Version> {
        (*self).get_version()
    }
}

impl Version for crates_io_api::Version {
    fn get_version(&self) -> Option<semver::Version> {
        // Remove leading `v` for git tags
        let ver_str = match self.num.strip_prefix('v') {
            Some(v) => v,
            None => &self.num,
        };

        // Parse out version
        semver::Version::parse(ver_str).ok()
    }
}

pub(super) fn find_version<Item: Version, VersionIter: Iterator<Item = Item>>(
    requirement: &str,
    version_iter: VersionIter,
) -> Result<(Item, semver::Version), BinstallError> {
    // Parse version requirement
    let version_req = VersionReq::parse(requirement).map_err(|err| BinstallError::VersionReq {
        req: requirement.into(),
        err,
    })?;

    version_iter
        // Filter for matching versions
        .filter_map(|item| {
            let ver = item.get_version()?;

            // Filter by version match
            if version_req.matches(&ver) {
                debug!("Version: {:?}", ver);
                Some((item, ver))
            } else {
                None
            }
        })
        // Return highest version
        .max_by_key(|(_item, ver)| ver.clone())
        .ok_or(BinstallError::VersionMismatch { req: version_req })
}
