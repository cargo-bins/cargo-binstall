//! Concrete Binstall operations.

use std::{path::PathBuf, sync::Arc};

use semver::VersionReq;

use crate::{
    fetchers::{Data, Fetcher},
    helpers::remote::Client,
    manifests::cargo_toml_binstall::PkgOverride,
    DesiredTargets,
};

pub mod install;
pub mod resolve;

pub type Resolver = fn(&Client, &Arc<Data>) -> Arc<dyn Fetcher>;

pub struct Options {
    pub no_symlinks: bool,
    pub dry_run: bool,
    pub force: bool,
    pub version_req: Option<VersionReq>,
    pub manifest_path: Option<PathBuf>,
    pub cli_overrides: PkgOverride,
    pub desired_targets: DesiredTargets,
    pub quiet: bool,
    pub resolver: Vec<Resolver>,
}
