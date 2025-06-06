//! Common structure for crate information for post-install manifests.

use std::{borrow, cmp, hash};

use compact_str::CompactString;
use maybe_owned::MaybeOwned;
use once_cell::sync::Lazy;
use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

pub fn cratesio_url() -> &'static Url {
    static CRATESIO: Lazy<Url, fn() -> Url> =
        Lazy::new(|| Url::parse("https://github.com/rust-lang/crates.io-index").unwrap());

    &CRATESIO
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrateInfo {
    pub name: CompactString,
    pub version_req: CompactString,
    pub current_version: Version,
    pub source: CrateSource,
    pub target: CompactString,
    pub bins: Vec<CompactString>,
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
        Some(self.cmp(other))
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
    Sparse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrateSource {
    pub source_type: SourceType,
    pub url: MaybeOwned<'static, Url>,
}

impl CrateSource {
    pub fn cratesio_registry() -> CrateSource {
        Self {
            source_type: SourceType::Registry,
            url: MaybeOwned::Borrowed(cratesio_url()),
        }
    }
}
