use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use binstalk::helpers::statics::cargo_home;
use log::debug;

/// Fetch install path from environment
/// roughly follows <https://doc.rust-lang.org/cargo/commands/cargo-install.html#description>
///
/// Return (install_path, is_custom_install_path)
pub fn get_install_path<P: AsRef<Path>>(install_path: Option<P>) -> (Option<Arc<Path>>, bool) {
    // Command line override first first
    if let Some(p) = install_path {
        return (Some(Arc::from(p.as_ref())), true);
    }

    // Environmental variables
    if let Ok(p) = std::env::var("CARGO_INSTALL_ROOT") {
        debug!("using CARGO_INSTALL_ROOT ({p})");
        let b = PathBuf::from(p);
        return (Some(Arc::from(b.join("bin"))), true);
    }

    if let Ok(p) = cargo_home() {
        debug!("using ({}) as cargo home", p.display());
        return (Some(p.join("bin").into()), false);
    }

    // Local executable dir if no cargo is found
    let dir = dirs::executable_dir();

    if let Some(d) = &dir {
        debug!("Fallback to {}", d.display());
    }

    (dir.map(Arc::from), true)
}
