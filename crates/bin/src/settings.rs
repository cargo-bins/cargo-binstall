use std::{
    fs::{create_dir_all, File},
    io::{Read, Write as _},
    path::PathBuf,
};

use binstalk::QUICKINSTALL_STATS_URL;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::args::{Args, StrategyWrapped};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Settings {
    #[serde(default = "tru")]
    pub confirm: bool,

    #[serde(default)]
    pub install_path: Option<PathBuf>,

    #[serde(default = "tru")]
    pub track_installs: bool,

    #[serde(default)]
    pub targets: Option<Vec<String>>,

    #[serde(default)]
    pub strategies: Vec<StrategyWrapped>,

    #[serde(default)]
    pub telemetry: Telemetry,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Telemetry {
    pub enabled: bool,
    pub consent: bool,
    pub endpoint: String,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            enabled: true,
            consent: false,
            endpoint: QUICKINSTALL_STATS_URL.into(),
        }
    }
}

impl Settings {
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
        if let Some(targets) = &args.targets {
            self.targets = Some(targets.clone());
        }
        if !args.strategies.is_empty() {
            self.strategies = args.strategies.clone();
        }
        self
    }
}

pub fn load(error_if_inaccessible: bool, path: PathBuf) -> Result<Settings> {
    fn inner(path: PathBuf) -> Result<Settings> {
        create_dir_all(
            path.parent()
                .ok_or_else(|| miette!("settings path has no parent"))?,
        )
        .into_diagnostic()
        .wrap_err("create settings directory")?;

        match File::options().create_new(true).open(&path) {
            Ok(mut file) => {
                debug!(?path, "creating new settings file");
                let settings = Settings::default();
                file.write_all(
                    toml::to_string_pretty(&settings)
                        .into_diagnostic()
                        .wrap_err("serialise default settings")?
                        .as_bytes(),
                )
                .into_diagnostic()
                .wrap_err("write default settings")?;
                Ok(settings)
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                debug!(?path, "loading binstall settings");
                let mut file = File::options()
                    .read(true)
                    .open(&path)
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
            Err(err) => {
                return Err(err)
                    .into_diagnostic()
                    .wrap_err("reading binstall settings file")
            }
        }
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

fn tru() -> bool {
    true
}
