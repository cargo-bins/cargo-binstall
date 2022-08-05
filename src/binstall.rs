use std::path::PathBuf;

use compact_str::CompactString;

use crate::{metafiles::binstall_v1::MetaData, DesiredTargets, PkgOverride};

mod resolve;
pub use resolve::*;

mod install;
pub use install::*;

pub struct Options {
    pub no_symlinks: bool,
    pub dry_run: bool,
    pub force: bool,
    pub version: Option<CompactString>,
    pub manifest_path: Option<PathBuf>,
    pub cli_overrides: PkgOverride,
    pub desired_targets: DesiredTargets,
    pub quiet: bool,
}
