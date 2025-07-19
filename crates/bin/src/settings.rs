use std::{
    fs::{create_dir_all, File},
    io::{Read, Write as _},
    path::PathBuf,
};

use binstalk::QUICKINSTALL_STATS_URL;
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Settings {
    #[serde(default)]
    pub confirm: bool,

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
                    toml::to_string(&settings)
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
