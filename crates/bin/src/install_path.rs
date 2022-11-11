use std::{
    env::var_os,
    path::{Path, PathBuf},
    sync::Arc,
};

use binstalk::home::cargo_home;
use tracing::debug;

pub fn get_cargo_roots_path(cargo_roots: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(p) = cargo_roots {
        return Some(p);
    }

    // Environmental variables
    if let Some(p) = var_os("CARGO_INSTALL_ROOT") {
        let p = PathBuf::from(p);
        debug!("using CARGO_INSTALL_ROOT ({})", p.display());
        return Some(p);
    }

    if let Ok(p) = cargo_home() {
        debug!("using ({}) as cargo home", p.display());
        Some(p)
    } else {
        None
    }
}

/// Fetch install path from environment
/// roughly follows <https://doc.rust-lang.org/cargo/commands/cargo-install.html#description>
///
/// Return (install_path, is_custom_install_path)
pub fn get_install_path<P: AsRef<Path>>(
    install_path: Option<P>,
    cargo_roots: Option<P>,
) -> (Option<Arc<Path>>, bool) {
    // Command line override first first
    if let Some(p) = install_path {
        return (Some(Arc::from(p.as_ref())), true);
    }

    // Then cargo_roots
    if let Some(p) = cargo_roots {
        return (Some(Arc::from(p.as_ref().join("bin"))), false);
    }

    // Local executable dir if no cargo is found
    let dir = dirs::executable_dir();

    if let Some(d) = &dir {
        debug!("Fallback to {}", d.display());
    }

    (dir.map(Arc::from), true)
}
