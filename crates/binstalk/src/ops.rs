//! Concrete Binstall operations.
//!
//! The install flow distinguishes between two destination roots:
//!
//! - `install_path` for executable artifacts that should appear on `PATH`
//! - `cargo_root` for Cargo-managed shared data such as man pages and shell
//!   completions
//!
//! This split is intentional. Callers may override where binaries are placed,
//! but extra files still need a stable, conventional home under Cargo's
//! `share/` tree so previewing, installation, and tracked cleanup all reason
//! about the same location.
//!
//! In particular, package metadata is allowed to describe where extra files
//! live inside an archive, but it does not choose arbitrary on-disk install
//! destinations. That keeps crate metadata focused on packaging layout while
//! binstall owns host policy such as "which root should man pages be installed
//! under?".
//!
//! This is an opinionated v1 design. The alternative would be to make extras
//! follow `install_path` or to allow package metadata to control destinations.
//! That would make the resulting layout less conventional and harder to track,
//! especially when binaries are installed into custom locations.

use std::{path::PathBuf, sync::Arc, time::Duration};

use compact_str::CompactString;

use crate::{
    fetchers::{Data, Fetcher, SignaturePolicy, TargetDataErased},
    helpers::{
        gh_api_client::GhApiClient, jobserver_client::LazyJobserverClient,
        lazy_gh_api_client::LazyGhApiClient, remote::Client,
    },
    manifests::cargo_toml_binstall::PkgOverride,
    registry::ResolvedRegistry,
    DesiredTargets,
};

pub mod resolve;

pub type Resolver =
    fn(Client, GhApiClient, Arc<Data>, Arc<TargetDataErased>, SignaturePolicy) -> Arc<dyn Fetcher>;

#[derive(Debug)]
#[non_exhaustive]
pub enum CargoTomlFetchOverride {
    #[cfg(feature = "git")]
    Git(crate::helpers::git::GitUrl),
    Path(PathBuf),
}

#[derive(Debug)]
pub struct Options {
    pub no_symlinks: bool,
    pub dry_run: bool,
    pub force: bool,
    pub quiet: bool,
    pub locked: bool,
    pub no_track: bool,

    pub cargo_toml_fetch_override: Option<CargoTomlFetchOverride>,
    pub cli_overrides: PkgOverride,

    pub desired_targets: DesiredTargets,
    pub resolvers: Vec<Resolver>,
    pub cargo_install_fallback: bool,

    /// If provided, the names are sorted.
    pub bins: Option<Vec<CompactString>>,

    pub temp_dir: PathBuf,
    /// Directory where executables are installed.
    ///
    /// This may differ from `cargo_root.join("bin")` when the user supplies a
    /// custom install path.
    ///
    /// Only executable artifacts use this root. Extra files continue to target
    /// `cargo_root` so they land in Cargo's shared-data layout rather than next
    /// to the binary.
    pub install_path: PathBuf,
    pub has_overriden_install_path: bool,
    /// Cargo-managed root used for manifests and shared install data.
    ///
    /// Extra files are rooted here even when binaries are installed elsewhere.
    /// This gives a single stable base for man pages, shell completions, and
    /// manifest-based stale-file cleanup.
    pub cargo_root: Option<PathBuf>,
    pub cargo_install_registry: Option<CompactString>,
    pub cargo_install_index: Option<CompactString>,

    pub client: Client,
    pub gh_api_client: LazyGhApiClient,
    pub jobserver_client: LazyJobserverClient,
    pub registry: ResolvedRegistry,

    pub signature_policy: SignaturePolicy,
    pub disable_telemetry: bool,

    pub maximum_resolution_timeout: Duration,
}
