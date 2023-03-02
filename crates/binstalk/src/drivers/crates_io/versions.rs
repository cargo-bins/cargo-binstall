use std::fmt;

use compact_str::CompactString;
use semver::VersionReq;
use serde::{
    de::{DeserializeSeed, Deserializer, Error, MapAccess, SeqAccess, Visitor},
    Deserialize,
};

use crate::{
    errors::BinstallError,
    helpers::remote::{Error as RemoteError, JsonDeserializer},
};

#[derive(Deserialize)]
struct Version {
    num: CompactString,
    yanked: bool,
}

/// Find the max version that is not yanked and matches `version_req`.
pub(super) fn find_max_version_matched(
    deserializer: &mut JsonDeserializer,
    version_req: &VersionReq,
) -> Result<CompactString, BinstallError> {
    struct VersionsFieldVisitor<'a>(&'a VersionReq);

    impl<'de> Visitor<'de> for VersionsFieldVisitor<'_> {
        type Value = Option<CompactString>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("The visitor expects a Visitor::visit_map to be called")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            while let Some(key) = map.next_key::<CompactString>()? {
                // Find key versions and deserialize the versions array

                if key != "versions" {
                    continue;
                }

                return map.next_value_seed(FindMaxVersionMatched(self.0));
            }

            Err(A::Error::missing_field("versions"))
        }
    }

    deserializer
        .deserialize_struct("Versions", &["versions"], VersionsFieldVisitor(version_req))
        .map_err(RemoteError::from)?
        .ok_or_else(|| BinstallError::VersionMismatch {
            req: version_req.clone(),
        })
}

struct FindMaxVersionMatched<'a>(&'a VersionReq);

impl<'de> DeserializeSeed<'de> for FindMaxVersionMatched<'_> {
    type Value = Option<CompactString>;

    /// Equivalent to
    ///
    /// ```ignore
    /// #[derive(Deserialize)]
    /// struct Versions {
    ///     versions: Vec<Version>,
    /// }
    ///
    /// let versions: Versions = ...;
    ///
    /// versions
    ///     .into_iter()
    ///     .filter_map(|item| {
    ///         if !item.yanked {
    ///             // Remove leading `v` for git tags
    ///             let ver = item.num.strip_prefix('v').unwrap_or(&item.num);
    ///
    ///             // Parse out version
    ///             let ver = semver::Version::parse(ver).ok()?;
    ///
    ///             // Filter by version match
    ///             version_req.matches(&ver).then_some((item.num, ver))
    ///         } else {
    ///             None
    ///         }
    ///     })
    ///     // Return highest version
    ///     .max_by(|(_ver_str_x, ver_x), (_ver_str_y, ver_y)| ver_x.cmp(ver_y))
    ///     .ok_or_else(|| BinstallError::VersionMismatch {
    ///         req: version_req.clone(),
    ///     })?
    ///     .0
    /// ```
    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VersionsVisitor<'a>(&'a VersionReq);

        impl<'de> Visitor<'de> for VersionsVisitor<'_> {
            type Value = Option<CompactString>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("The visitor expects a Visitor::visit_seq to be called")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut max_version_str = None;
                let mut max_version = None;

                while let Some(Version { num, yanked }) = seq.next_element::<Version>()? {
                    if yanked {
                        continue;
                    }

                    // Remove leading `v` for git tags
                    let ver = num.strip_prefix('v').unwrap_or(&num);

                    // Parse the version
                    let Ok(ver) = semver::Version::parse(ver) else { continue };

                    // Filter by version match
                    if !self.0.matches(&ver) {
                        continue;
                    }

                    if max_version
                        .as_ref()
                        .map(|max_ver| max_ver < &ver)
                        .unwrap_or(true)
                    {
                        max_version = Some(ver);
                        max_version_str = Some(num);
                    }
                }

                Ok(max_version_str)
            }
        }

        deserializer.deserialize_seq(VersionsVisitor(self.0))
    }
}
