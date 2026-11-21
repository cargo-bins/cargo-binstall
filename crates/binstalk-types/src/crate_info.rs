//! Common structure for crate information for post-install manifests.

use std::{borrow, cmp, hash, sync::LazyLock};

use compact_str::CompactString;
use maybe_owned::MaybeOwned;
use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::cargo_toml_binstall::PkgFmt;

pub fn cratesio_url() -> &'static Url {
    static CRATESIO: LazyLock<Url> =
        LazyLock::new(|| Url::parse("https://github.com/rust-lang/crates.io-index").unwrap());

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
    /// How the package was found on `target`, when it came from a default
    /// `pkg-url`, so that updating it can try that first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pkg_url_hint: Option<PkgUrlHint>,
}

/// A default `pkg-url` that a crate's package was found with.
///
/// Binstall otherwise probes every default `pkg-url` x `pkg-fmt` combination
/// on each install, which is a lot of requests and GitHub API usage for an
/// update whose asset naming almost never changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkgUrlHint {
    /// The name `{ name }` was rendered with: the crate's, or its only
    /// binary's.
    pub name: CompactString,
    /// The template, in the same syntax as `pkg-url` in `Cargo.toml`.
    pub pkg_url: CompactString,
    pub pkg_fmt: PkgFmt,
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
