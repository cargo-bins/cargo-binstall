//! Concrete Binstall operations.

use std::path::PathBuf;

use semver::VersionReq;

use crate::{manifests::cargo_toml_binstall::PkgOverride, targets::DesiredTargets};

pub mod install;
pub mod resolve;

pub struct Options {
    pub no_symlinks: bool,
    pub dry_run: bool,
    pub force: bool,
    pub version_req: Option<VersionReq>,
    pub manifest_path: Option<PathBuf>,
    pub cli_overrides: PkgOverride,
    pub desired_targets: DesiredTargets,
    pub quiet: bool,
}
