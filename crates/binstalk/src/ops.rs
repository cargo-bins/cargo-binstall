//! Concrete Binstall operations.

use std::{path::PathBuf, sync::Arc};

use crates_io_api::AsyncClient as CratesIoApiClient;
use semver::VersionReq;

use crate::{
    fetchers::{Data, Fetcher, TargetData},
    helpers::{jobserver_client::LazyJobserverClient, remote::Client},
    manifests::cargo_toml_binstall::PkgOverride,
    DesiredTargets,
};

pub mod install;
pub mod resolve;

pub type Resolver = fn(Client, Arc<Data>, Arc<TargetData>) -> Arc<dyn Fetcher>;

pub struct Options {
    pub no_symlinks: bool,
    pub dry_run: bool,
    pub force: bool,
    pub quiet: bool,

    pub version_req: Option<VersionReq>,
    pub manifest_path: Option<PathBuf>,
    pub cli_overrides: PkgOverride,

    pub desired_targets: DesiredTargets,
    pub resolvers: Vec<Resolver>,
    pub cargo_install_fallback: bool,

    pub temp_dir: PathBuf,
    pub install_path: PathBuf,
    pub client: Client,
    pub crates_io_api_client: CratesIoApiClient,
    pub jobserver_client: LazyJobserverClient,
}
