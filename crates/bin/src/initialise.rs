use std::{borrow::Cow, env::var_os, fs, path::PathBuf};

use binstalk::errors::BinstallError;
use binstalk_manifests::{cargo_config::Config as CargoConfig, crates_manifests::Manifests};
use home::cargo_home;
use miette::{bail, Result, WrapErr};
use tempfile::TempDir;
use tracing::{debug, info};

use crate::{args::Args, ui::confirm_sync};

pub(crate) struct Init {
    pub(crate) cargo_config: CargoConfig,
    pub(crate) settings: crate::settings::Settings,
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

    let (cargo_root, is_cargo_root_overridden) = if let Some(p) = &args.root {
        debug!(path=?p, "install root from --root");
        (p.into(), true)
    } else if let Some(p) = var_os("CARGO_INSTALL_ROOT").map(PathBuf::from) {
        debug!(path=?p, "install root from CARGO_INSTALL_ROOT");
        (p, true)
    } else if let Some(p) = cargo_config
        .as_ref()
        .and_then(|config| config.install.as_ref())
        .and_then(|install| install.root.clone())
    {
        debug!(path=?p, "install root from cargo config");
        (p, false)
    } else if let Some(p) = &cargo_home {
        debug!(path=?p, "install root from cargo home");
        (p.into(), false)
    } else if let Some(p) = dirs::executable_dir() {
        debug!(path=?p, "install root from executable dir");
        (p, false)
    } else {
        bail!("No install root found, provide one with --root or CARGO_INSTALL_ROOT");
    };

    let (cargo_root, cargo_config) = if let Some(config) = cargo_config {
        (cargo_root, config)
    } else {
        let config = CargoConfig::load_from_path(cargo_root.join("config.toml"))?;
        (
            match config
                .install
                .as_ref()
                .and_then(|install| install.root.as_ref())
            {
                Some(root) if !is_cargo_root_overridden => root.clone(),
                _ => cargo_root,
            },
            config,
        )
    };

    let settings_path = args
        .settings
        .as_deref()
        .map(Cow::Borrowed)
        .unwrap_or_else(|| {
            Cow::Owned(
                cargo_home
                    .as_ref()
                    .unwrap_or(&cargo_root)
                    .join("binstall.toml"),
            )
        });
    let mut settings = crate::settings::load(args.settings.is_some(), &settings_path)?;

    #[allow(clippy::print_literal)]
    if !args.self_install
        && !args.disable_telemetry
        && !args.no_confirm
        && !settings.telemetry.consent_asked
    {
        info!(url=?binstalk::QUICKINSTALL_STATS_URL, "the current QuickInstall statistics endpoint");
        eprintln!(
            "\n{}\n{}\n{}\n{}",
            "Binstall would like to collect install statistics for the QuickInstall project",
            "to help inform which packages should be included in its index in the future.",
            "If you agree, please type 'yes'. If you disagree, telemetry will not be sent.",
            "You can change this at any time by editing the binstall settings file.",
        );
        settings.telemetry_consent(confirm_sync("Opt in to telemetry? yes/[no] ", false));
        settings.save(&settings_path)?;
        info!(path=?settings_path, "Settings saved");
    }

    let (install_path, custom_install_path) = if let Some(p) = &args.install_path {
        debug!(path=?p, "install path from --install-path");
        (p.into(), true)
    } else if let Some(p) = &settings.install_path {
        debug!(path=?p, "install path from settings");
        (p.into(), true)
    } else if cargo_home.is_some() {
        let p = cargo_root.join("bin");
        debug!(path=?p, "install path from cargo root");
        (p, false)
    } else if let Some(p) = dirs::executable_dir() {
        debug!(path=?p, "install path from executable dir");
        (p, true)
    } else {
        bail!("Could not determine installation path. Provide one with --install-path")
    };

    let settings = settings.merge_args(args);

    fs::create_dir_all(&cargo_root).map_err(BinstallError::Io)?;
    fs::create_dir_all(&install_path).map_err(BinstallError::Io)?;

    let manifests = if settings.track_installs && !custom_install_path {
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
        settings,
        cargo_root,
        install_path,
        manifests,
        temp_dir,
    })
}
