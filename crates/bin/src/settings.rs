use std::{
    fs::{create_dir_all, File},
    io::{self, Read, Write as _},
    path::{Path, PathBuf},
};

use fs_lock::FileLock;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::args::{Args, StrategyWrapped};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub confirm: bool,
    pub install_path: Option<PathBuf>,
    pub track_installs: bool,
    pub continue_on_failure: bool,
    pub targets: Option<Vec<String>>,
    pub strategies: Vec<StrategyWrapped>,
    pub telemetry: Telemetry,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            confirm: true,
            install_path: None,
            track_installs: true,
            continue_on_failure: false,
            targets: None,
            strategies: vec![],
            telemetry: Telemetry::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Telemetry {
    pub enabled: bool,
    pub consent_asked: bool,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            enabled: true,
            consent_asked: false,
        }
    }
}

impl Settings {
    pub(crate) fn telemetry_consent(&mut self, enable: bool) {
        self.telemetry.consent_asked = true;
        self.telemetry.enabled = enable;
    }

    pub(crate) fn merge_args(mut self, args: &Args) -> Self {
        if let Some(path) = &args.install_path {
            self.install_path = Some(path.clone());
        }
        if args.no_confirm {
            self.confirm = false;
        }
        if args.disable_telemetry {
            self.telemetry.enabled = false;
        }
        if args.no_track {
            self.track_installs = false;
        }
        if args.continue_on_failure {
            self.continue_on_failure = true;
        }
        if let Some(targets) = &args.targets {
            self.targets = Some(targets.clone());
        }
        if !args.strategies.is_empty() {
            self.strategies = args.strategies.clone();
        }
        if !self.telemetry.consent_asked {
            self.telemetry.enabled = false;
        }
        self
    }

    fn write(&self, file: &mut File) -> Result<()> {
        file.write_all(
            toml::to_string_pretty(self)
                .into_diagnostic()
                .wrap_err("serialise default settings")?
                .as_bytes(),
        )
        .into_diagnostic()
        .wrap_err("write default settings")
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let mut file = File::options()
            .create(true)
            .write(true)
            .append(false)
            .truncate(true)
            .open(path)
            .and_then(FileLock::new_exclusive)
            .into_diagnostic()
            .wrap_err("open settings file")?;
        self.write(&mut file)
    }
}

pub fn load(error_if_inaccessible: bool, path: &Path) -> Result<Settings> {
    fn inner(path: &Path) -> Result<Settings> {
        create_dir_all(
            path.parent()
                .ok_or_else(|| miette!("settings path has no parent"))?,
        )
        .into_diagnostic()
        .wrap_err("create settings directory")?;

        debug!(?path, "checking if settings file exists");
        if !path.exists() {    
            debug!(?path, "trying to create new settings file");
            let mut file = match File::create_new(path) {
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                res => {
                    res
                        .and_then(FileLock::new_exclusive)
                        .into_diagnostic()
                        .wrap_err("creating new settings file")?;
                    debug!(?path, "writing new settings file");
                   let settings = Settings::default();
                   settings.write(&mut file)?;
                   return Ok(settings);
                }
            };
        }

        debug!(?path, "loading binstall settings");
        let mut file = File::open(path)
            .and_then(FileLock::new_shared)
            .into_diagnostic()
            .wrap_err("open existing settings file")?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .into_diagnostic()
            .wrap_err("read existing settings file")?;

        let settings = toml::from_str(&contents)
            .into_diagnostic()
            .wrap_err("parse existing settings file")?;

        debug!(?settings, "loaded binstall settings");
        Ok(settings)
    }

    let settings = inner(path);
    if error_if_inaccessible {
        settings
    } else {
        Ok(settings
            .inspect_err(|err| {
                warn!("{err:?}");
            })
            .unwrap_or_default())
    }
}
