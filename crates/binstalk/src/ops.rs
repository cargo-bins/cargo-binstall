//! Concrete Binstall operations.

use std::{path::PathBuf, sync::Arc};

use semver::VersionReq;

use crate::{
    fetchers::{Data, Fetcher, SignaturePolicy, TargetDataErased},
    helpers::{
        self, cacher::HTTPCacher, gh_api_client::GhApiClient,
        jobserver_client::LazyJobserverClient, remote::Client,
    },
    manifests::cargo_toml_binstall::PkgOverride,
    registry::Registry,
    DesiredTargets,
};

pub mod resolve;

pub type Resolver = fn(
    Client,
    GhApiClient,
    HTTPCacher,
    Arc<Data>,
    Arc<TargetDataErased>,
    SignaturePolicy,
) -> Arc<dyn Fetcher>;

#[derive(Debug)]
#[non_exhaustive]
pub enum CargoTomlFetchOverride {
    #[cfg(feature = "git")]
    Git(helpers::git::GitUrl),
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

    pub version_req: Option<VersionReq>,
    pub cargo_toml_fetch_override: Option<CargoTomlFetchOverride>,
    pub cli_overrides: PkgOverride,

    pub desired_targets: DesiredTargets,
    pub resolvers: Vec<Resolver>,
    pub cargo_install_fallback: bool,

    pub temp_dir: PathBuf,
    pub install_path: PathBuf,
    pub cargo_root: Option<PathBuf>,

    pub client: Client,
    pub gh_api_client: GhApiClient,
    pub cacher: HTTPCacher,
    pub jobserver_client: LazyJobserverClient,
    pub registry: Registry,

    pub signature_policy: SignaturePolicy,
}
