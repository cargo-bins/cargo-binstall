use std::{
    env::var_os,
    path::{Path, PathBuf},
};

use binstalk_manifests::cargo_config::Config;
use tracing::debug;

pub fn get_cargo_roots_path(
    cargo_roots: Option<PathBuf>,
    cargo_home: PathBuf,
    config: &mut Config,
) -> Option<PathBuf> {
    if let Some(p) = cargo_roots {
        Some(p)
    } else if let Some(p) = var_os("CARGO_INSTALL_ROOT") {
        // Environmental variables
        let p = PathBuf::from(p);
        debug!("using CARGO_INSTALL_ROOT ({})", p.display());
        Some(p)
    } else if let Some(root) = config.install.take().and_then(|install| install.root) {
        debug!("using `install.root` {} from cargo config", root.display());
        Some(root)
    } else {
        debug!("using ({}) as cargo home", cargo_home.display());
        Some(cargo_home)
    }
}

/// Fetch install path from environment
/// roughly follows <https://doc.rust-lang.org/cargo/commands/cargo-install.html#description>
///
/// Return (install_path, is_custom_install_path)
pub fn get_install_path(
    install_path: Option<PathBuf>,
    cargo_roots: Option<impl AsRef<Path>>,
) -> (Option<PathBuf>, bool) {
    // Command line override first first
    if let Some(p) = install_path {
        return (Some(p), true);
    }

    // Then cargo_roots
    if let Some(p) = cargo_roots {
        return (Some(p.as_ref().join("bin")), false);
    }

    // Local executable dir if no cargo is found
    let dir = dirs::executable_dir();

    if let Some(d) = &dir {
        debug!("Fallback to {}", d.display());
    }

    (dir, true)
}
