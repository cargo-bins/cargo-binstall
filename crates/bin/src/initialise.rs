use std::{env::var_os, fs, path::PathBuf};

use binstalk::errors::BinstallError;
use binstalk_manifests::{cargo_config::Config as CargoConfig, crates_manifests::Manifests};
use home::cargo_home;
use miette::{bail, Result, WrapErr};
use tempfile::TempDir;
use tracing::debug;

use crate::args::Args;

pub(crate) struct Init {
    pub(crate) cargo_config: CargoConfig,
    pub(crate) cargo_root: PathBuf,
    pub(crate) install_path: PathBuf,
    pub(crate) manifests: Option<Manifests>,
    pub(crate) temp_dir: TempDir,
}

pub(crate) fn initialise(args: &Args) -> Result<Init> {
    let (cargo_config, cargo_home) = if let Ok(home) = cargo_home() {
        (
            Some(CargoConfig::load_from_path(home.join("config.toml"))?),
            Some(home),
        )
    } else {
        (None, None)
    };

    let cargo_root = if let Some(p) = &args.root {
        debug!(path=?p, "install root from --root");
        p.into()
    } else if let Some(p) = var_os("CARGO_INSTALL_ROOT").map(PathBuf::from) {
        debug!(path=?p, "install root from CARGO_INSTALL_ROOT");
        p
    } else if let Some(p) = cargo_config
        .as_ref()
        .and_then(|config| config.install.clone().and_then(|install| install.root))
    {
        debug!(path=?p, "install root from cargo config");
        p
    } else if let Some(p) = &cargo_home {
        debug!(path=?p, "install root from cargo home");
        p.into()
    } else if let Some(p) = dirs::executable_dir() {
        debug!(path=?p, "install root from executable dir");
        p
    } else {
        bail!("No install root found, provide one with --root or CARGO_INSTALL_ROOT");
    };

    let cargo_config = if let Some(config) = cargo_config {
        config
    } else {
        CargoConfig::load_from_path(cargo_root.join("config.toml"))?
    };

    let (install_path, custom_install_path) = if let Some(p) = &args.install_path {
        debug!(path=?p, "install path from --install-path");
        (p.into(), true)
    } else if let Some(p) = dirs::executable_dir() {
        debug!(path=?p, "install path from executable dir");
        (p, true)
    } else {
        let p = cargo_root.join("bin");
        debug!(path=?p, "install path from cargo root");
        (p, false)
    };

    fs::create_dir_all(&cargo_root).map_err(BinstallError::Io)?;
    fs::create_dir_all(&install_path).map_err(BinstallError::Io)?;

    let manifests = if !(args.no_track || custom_install_path) {
        Some(Manifests::open_exclusive(&cargo_root)?)
    } else {
        None
    };

    // Create a temporary directory for downloads etc.
    //
    // Put all binaries to a temporary directory under `dst` first, catching
    // some failure modes (e.g., out of space) before touching the existing
    // binaries. This directory will get cleaned up via RAII.
    let temp_dir = tempfile::Builder::new()
        .prefix("cargo-binstall")
        .tempdir_in(&cargo_root)
        .map_err(BinstallError::from)
        .wrap_err("Creating a temporary directory failed.")?;

    Ok(Init {
        cargo_config,
        cargo_root,
        install_path,
        manifests,
        temp_dir,
    })
}
