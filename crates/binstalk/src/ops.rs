//! Concrete Binstall operations.

use std::{path::PathBuf, sync::Arc};

use semver::VersionReq;
use tokio::{sync::Mutex, time::Interval};

use crate::{
    fetchers::{Data, Fetcher, TargetData},
    helpers::{gh_api_client::GhApiClient, jobserver_client::LazyJobserverClient, remote::Client},
    manifests::cargo_toml_binstall::PkgOverride,
    DesiredTargets,
};

pub mod resolve;

pub type Resolver = fn(Client, GhApiClient, Arc<Data>, Arc<TargetData>) -> Arc<dyn Fetcher>;

pub struct Options {
    pub no_symlinks: bool,
    pub dry_run: bool,
    pub force: bool,
    pub quiet: bool,
    pub locked: bool,

    pub version_req: Option<VersionReq>,
    pub manifest_path: Option<PathBuf>,
    pub cli_overrides: PkgOverride,

    pub desired_targets: DesiredTargets,
    pub resolvers: Vec<Resolver>,
    pub cargo_install_fallback: bool,

    pub temp_dir: PathBuf,
    pub install_path: PathBuf,
    pub cargo_root: Option<PathBuf>,

    pub client: Client,
    pub gh_api_client: GhApiClient,
    pub jobserver_client: LazyJobserverClient,
    pub crates_io_rate_limit: Mutex<Interval>,
}
