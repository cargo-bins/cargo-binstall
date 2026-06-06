//! Concrete Binstall operations.

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

    /// Features to activate when the `compile` (cargo install) strategy is used.
    ///
    /// These are passed through verbatim to `cargo install --features <value>` (one
    /// argument per element). When a prebuilt fetcher is selected instead, binstall
    /// will log a warning that the feature list cannot be enforced.
    pub features: Vec<CompactString>,

    /// When true, the `compile` strategy will be invoked with
    /// `--no-default-features`. Callers are responsible for ensuring the compile
    /// strategy is actually reachable (e.g. by removing prebuilt resolvers from
    /// `resolvers`). Ignored when `all_features == true`, matching cargo's
    /// mutual exclusion of the two flags.
    pub no_default_features: bool,

    /// When true, the `compile` strategy will be invoked with `--all-features`.
    /// Callers are responsible for ensuring the compile strategy is actually
    /// reachable (e.g. by removing prebuilt resolvers from `resolvers`).
    /// Takes precedence over `no_default_features`.
    pub all_features: bool,

    pub temp_dir: PathBuf,
    pub install_path: PathBuf,
    pub has_overriden_install_path: bool,
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
