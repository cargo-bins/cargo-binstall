//! Concrete Binstall operations.

use std::{path::PathBuf, sync::Arc};

use semver::VersionReq;
use tokio::{
    sync::Mutex,
    time::{interval, Duration, Interval, MissedTickBehavior},
};

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
    pub crates_io_rate_limit: CratesIoRateLimit,
}

pub struct CratesIoRateLimit(Mutex<Interval>);

impl Default for CratesIoRateLimit {
    fn default() -> Self {
        let mut interval = interval(Duration::from_secs(1));
        // If somehow one tick is delayed, then next tick should be at least
        // 1s later than the current tick.
        //
        // Other MissedTickBehavior including Burst (default), which will
        // tick as fast as possible to catch up, and Skip, which will
        // skip the current tick for the next one.
        //
        // Both Burst and Skip is not the expected behavior for rate limit:
        // ticking as fast as possible would violate crates.io crawler
        // policy, and skipping the current one will slow down the resolution
        // process.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self(Mutex::new(interval))
    }
}

impl CratesIoRateLimit {
    pub(super) async fn tick(&self) {
        self.0.lock().await.tick().await;
    }
}
