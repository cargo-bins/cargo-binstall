//! Cargo's `.cargo/config.toml`
//!
//! This manifest is used by Cargo to load configurations stored by users.
//!
//! Binstall reads from them to be compatible with `cargo-install`'s behavior.

use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

use compact_str::CompactString;
use fs_lock::FileLock;
use home::cargo_home;
use miette::Diagnostic;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Default, Deserialize)]
pub struct Install {
    /// `cargo install` destination directory
    pub root: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Http {
    /// HTTP proxy in libcurl format: "host:port"
    pub proxy: Option<CompactString>,
    /// timeout for each HTTP request, in seconds
    pub timeout: Option<u64>,
    /// path to Certificate Authority (CA) bundle
    pub cainfo: Option<PathBuf>,
    // TODO:
    // Support field ssl-version, ssl-version.max, ssl-version.min,
    // which needs `toml_edit::Item`.
}

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub install: Install,
    pub http: Http,
    // TODO:
    // Add support for section patch, source and registry for alternative
    // crates.io registry.

    // TODO:
    // Add field env for specifying env vars
    // which needs `toml_edit::Item`.
}

fn join_if_relative(path: &mut Option<PathBuf>, dir: &Path) {
    match path {
        Some(path) if path.is_relative() => *path = dir.join(&path),
        _ => (),
    }
}

impl Config {
    pub fn default_path() -> Result<PathBuf, ConfigLoadError> {
        Ok(cargo_home()?.join("config.toml"))
    }

    pub fn load() -> Result<Self, ConfigLoadError> {
        Self::load_from_path(Self::default_path()?)
    }

    /// * `dir` - path to the dir where the config.toml is located.
    ///           For relative path in the config, `Config::load_from_reader`
    ///           will join the `dir` and the relative path to form the final
    ///           path.
    pub fn load_from_reader<R: io::Read>(
        mut reader: R,
        dir: &Path,
    ) -> Result<Self, ConfigLoadError> {
        fn inner(reader: &mut dyn io::Read, dir: &Path) -> Result<Config, ConfigLoadError> {
            let mut vec = Vec::new();
            reader.read_to_end(&mut vec)?;

            if vec.is_empty() {
                Ok(Default::default())
            } else {
                let mut config: Config = toml_edit::de::from_slice(&vec)?;
                join_if_relative(&mut config.install.root, dir);
                join_if_relative(&mut config.http.cainfo, dir);
                Ok(config)
            }
        }

        inner(&mut reader, dir)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigLoadError> {
        let path = path.as_ref();
        let file = FileLock::new_shared(File::open(path)?)?;
        // Any regular file must have a parent dir
        Self::load_from_reader(file, path.parent().unwrap())
    }
}

#[derive(Debug, Diagnostic, Error)]
#[non_exhaustive]
pub enum ConfigLoadError {
    #[error("I/O Error: {0}")]
    Io(#[from] io::Error),

    #[error("Failed to deserialize toml: {0}")]
    TomlParse(Box<toml_edit::de::Error>),
}

impl From<toml_edit::de::Error> for ConfigLoadError {
    fn from(e: toml_edit::de::Error) -> Self {
        ConfigLoadError::TomlParse(Box::new(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;

    const CONFIG: &str = r#"
[env]
# Set ENV_VAR_NAME=value for any process run by Cargo
ENV_VAR_NAME = "value"
# Set even if already present in environment
ENV_VAR_NAME_2 = { value = "value", force = true }
# Value is relative to .cargo directory containing `config.toml`, make absolute
ENV_VAR_NAME_3 = { value = "relative/path", relative = true }

[http]
debug = false               # HTTP debugging
proxy = "host:port"         # HTTP proxy in libcurl format
timeout = 30                # timeout for each HTTP request, in seconds
cainfo = "cert.pem"         # path to Certificate Authority (CA) bundle

[install]
root = "/some/path"         # `cargo install` destination directory
    "#;

    #[test]
    fn test_loading() {
        let config = Config::load_from_reader(Cursor::new(&CONFIG), Path::new("/root")).unwrap();

        assert_eq!(
            config.install.root.as_deref().unwrap(),
            Path::new("/some/path")
        );
        assert_eq!(
            config.http.proxy,
            Some(CompactString::new_inline("host:port"))
        );

        assert_eq!(config.http.timeout, Some(30));
        assert_eq!(
            config.http.cainfo.as_deref().unwrap(),
            Path::new("/root/cert.pem")
        );
    }
}
