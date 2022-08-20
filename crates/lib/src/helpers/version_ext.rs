use compact_str::format_compact;
use semver::{Prerelease, Version, VersionReq};

/// Extension trait for [`VersionReq`].
pub trait VersionReqExt {
    /// Return `true` if `self.matches(version)` returns `true`
    /// and the `version` is the latest one acceptable by `self`.
    fn is_latest_compatible(&self, version: &Version) -> bool;
}

impl VersionReqExt for VersionReq {
    fn is_latest_compatible(&self, version: &Version) -> bool {
        if !self.matches(version) {
            return false;
        }

        // Test if bumping patch will be accepted
        let bumped_version = Version::new(version.major, version.minor, version.patch + 1);

        if self.matches(&bumped_version) {
            return false;
        }

        // Test if bumping prerelease will be accepted if version has one.
        let pre = &version.pre;
        if !pre.is_empty() {
            // Bump pre by appending random number to the end.
            let bumped_pre = format_compact!("{}.1", pre.as_str());

            let bumped_version = Version {
                major: version.major,
                minor: version.minor,
                patch: version.patch,
                pre: Prerelease::new(&bumped_pre).unwrap(),
                build: Default::default(),
            };

            if self.matches(&bumped_version) {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test() {
        // Test star
        assert!(!VersionReq::STAR.is_latest_compatible(&Version::parse("0.0.1").unwrap()));
        assert!(!VersionReq::STAR.is_latest_compatible(&Version::parse("0.1.1").unwrap()));
        assert!(!VersionReq::STAR.is_latest_compatible(&Version::parse("0.1.1-alpha").unwrap()));

        // Test ^x.y.z
        assert!(!VersionReq::parse("^0.1")
            .unwrap()
            .is_latest_compatible(&Version::parse("0.1.99").unwrap()));

        // Test =x.y.z
        assert!(VersionReq::parse("=0.1.0")
            .unwrap()
            .is_latest_compatible(&Version::parse("0.1.0").unwrap()));

        // Test =x.y.z-alpha
        assert!(VersionReq::parse("=0.1.0-alpha")
            .unwrap()
            .is_latest_compatible(&Version::parse("0.1.0-alpha").unwrap()));

        // Test >=x.y.z-alpha
        assert!(!VersionReq::parse(">=0.1.0-alpha")
            .unwrap()
            .is_latest_compatible(&Version::parse("0.1.0-alpha").unwrap()));
    }
}
