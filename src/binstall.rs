use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use reqwest::Client;

use crate::{metafiles::binstall_v1::MetaData, DesiredTargets, LazyJobserverClient, PkgOverride};

mod resolve;
pub use resolve::*;

mod install;
pub use install::*;

#[derive(Debug, Clone)]
pub struct Options {
    pub versioned: bool,
    pub dry_run: bool,
    pub version: Option<String>,
    pub manifest_path: Option<PathBuf>,
    pub cli_overrides: PkgOverride,
    pub desired_targets: DesiredTargets,
}

#[derive(Clone)]
pub struct Context {
    pub opts: Arc<Options>,
    pub temp_dir: Arc<Path>,
    pub install_path: Arc<Path>,
    pub client: Client,
    pub crates_io_api_client: crates_io_api::AsyncClient,
    pub jobserver_client: LazyJobserverClient,
}
