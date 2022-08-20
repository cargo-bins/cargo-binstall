//! Common structure for crate information for post-install manifests.

use std::{borrow, cmp, hash};

use compact_str::CompactString;
use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::helpers::statics::cratesio_url;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrateInfo {
    pub name: CompactString,
    pub version_req: CompactString,
    pub current_version: Version,
    pub source: CrateSource,
    pub target: CompactString,
    pub bins: Vec<CompactString>,

    /// Forwards compatibility. Unknown keys from future versions
    /// will be stored here and retained when the file is saved.
    ///
    /// We use an `Vec` here since it is never accessed in Rust.
    #[serde(flatten, with = "tuple_vec_map")]
    pub other: Vec<(CompactString, serde_json::Value)>,
}

impl borrow::Borrow<str> for CrateInfo {
    fn borrow(&self) -> &str {
        &self.name
    }
}

impl PartialEq for CrateInfo {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for CrateInfo {}

impl PartialOrd for CrateInfo {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        self.name.partial_cmp(&other.name)
    }
}

impl Ord for CrateInfo {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl hash::Hash for CrateInfo {
    fn hash<H>(&self, state: &mut H)
    where
        H: hash::Hasher,
    {
        self.name.hash(state)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub enum SourceType {
    Git,
    Path,
    Registry,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrateSource {
    pub source_type: SourceType,
    pub url: Url,
}

impl CrateSource {
    pub fn cratesio_registry() -> CrateSource {
        Self {
            source_type: SourceType::Registry,
            url: cratesio_url().clone(),
        }
    }
}
