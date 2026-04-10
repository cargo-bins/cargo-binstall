//! Cargo's `.cargo/config.toml`
//!
//! This manifest is used by Cargo to load configurations stored by users.
//!
//! Binstall reads from them to be compatible with `cargo-install`'s behavior.

use std::{
    borrow::Cow,
    collections::{BTreeMap, vec_deque::VecDeque},
    fs::File,
    io, mem,
    path::{Path, PathBuf},
};

use compact_str::CompactString;
use fs_lock::FileLock;
use home::cargo_home;
use miette::Diagnostic;
use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize)]
pub struct Install {
    /// `cargo install` destination directory
    pub root: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Http {
    /// HTTP proxy in libcurl format: "host:port"
    ///
    /// env: CARGO_HTTP_PROXY or HTTPS_PROXY or https_proxy or http_proxy
    pub proxy: Option<CompactString>,
    /// timeout for each HTTP request, in seconds
    ///
    /// env: CARGO_HTTP_TIMEOUT or HTTP_TIMEOUT
    pub timeout: Option<u64>,
    /// path to Certificate Authority (CA) bundle
    pub cainfo: Option<PathBuf>,
}

#[derive(Eq, PartialEq, Debug, Deserialize)]
#[serde(untagged)]
pub enum Env {
    Value(CompactString),
    WithOptions {
        value: CompactString,
        force: Option<bool>,
        relative: Option<bool>,
    },
}

#[derive(Debug, Deserialize)]
pub struct Registry {
    pub index: Option<CompactString>,
    #[serde(rename = "replace-with")]
    pub replace_with: Option<CompactString>,
    #[serde(rename = "credential-provider")]
    pub credential_provider: Option<CredentialProvider>,
}

#[derive(Debug, Deserialize)]
pub struct DefaultRegistry {
    pub default: Option<CompactString>,
    #[serde(rename = "credential-provider")]
    pub credential_provider: Option<CredentialProvider>,
    #[serde(rename = "global-credential-providers")]
    pub global_credential_providers: Option<Vec<CompactString>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum CredentialProvider {
    String(CompactString),
    Array(Vec<CompactString>),
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum IncludedConfig {
    Path(PathBuf),
    Extended {
        path: PathBuf,
        #[serde(default)]
        optional: bool,
    },
}

impl IncludedConfig {
    pub fn path(&self) -> &Path {
        match self {
            Self::Path(path) => path,
            Self::Extended { path, .. } => path,
        }
    }

    pub fn path_mut(&mut self) -> &mut Path {
        match self {
            Self::Path(path) => path,
            Self::Extended { path, .. } => path,
        }
    }

    pub fn optional(&self) -> bool {
        match self {
            Self::Path(..) => false,
            Self::Extended { optional, .. } => optional,
        }
    }

    fn load(&self) -> Result<Option<Config>, ConfigLoadError> {
        match File::open(self.path()) {
            Ok(file) => {
                let file = FileLock::new_shared(file)?.set_file_path(path);
                // Any regular file must have a parent dir
                //
                // Avoid automatically load included configs to avoid blowing
                // up the stack.
                Config::load_from_reader_inner(file, path.parent().unwrap()).map(Some)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound && self.optional() => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive] I
pub struct Config {
    pub install: Option<Install>,
    pub http: Option<Http>,
    pub env: Option<BTreeMap<CompactString, Env>>,
    pub registries: Option<BTreeMap<CompactString, Registry>>,
    pub registry: Option<DefaultRegistry>,
    #[serde(default)]
    pub include: Vec<IncludedConfig>,
    #[serde(rename = "credential-alias")]
    pub credential_alias: Option<BTreeMap<CompactString, CredentialProvider>>,
}

fn join_if_relative(path: Option<&mut PathBuf>, dir: &Path) {
    match path {
        Some(path) if path.is_relative() => *path = dir.join(&*path),
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

    fn load_from_reader_inner(reader: &mut dyn io::Read, dir: &Path) -> Result<Self, ConfigLoadError> {
        let mut vec = Vec::new();
        reader.read_to_end(&mut vec)?;

        if vec.is_empty() {
            Ok(Default::default())
        } else {
            let mut config: Config = toml_edit::de::from_slice(&vec)?;
            join_if_relative(
                config
                    .install
                    .as_mut()
                    .and_then(|install| install.root.as_mut()),
                dir,
            );
            join_if_relative(
                config.http.as_mut().and_then(|http| http.cainfo.as_mut()),
                dir,
            );
            if let Some(envs) = config.env.as_mut() {
                for env in envs.values_mut() {
                    if let Env::WithOptions {
                        value,
                        relative: Some(true),
                        ..
                    } = env
                    {
                        let path = Cow::Borrowed(Path::new(&value));
                        if path.is_relative() {
                            *value = dir.join(&path).to_string_lossy().into();
                        }
                    }
                }
            }

            for included_config in &mut config.include {
                join_if_relative(Some(included_config.path_mut()), dir);
            }
    
            Ok(config)
        }
    }

    /// * `dir` - path to the dir where the config.toml is located.
    ///   For relative path in the config, `Config::load_from_reader`
    ///   will join the `dir` and the relative path to form the final
    ///   path.
    pub fn load_from_reader<R: io::Read>(
        mut reader: R,
        dir: &Path,
    ) -> Result<Self, ConfigLoadError> {
        fn inner(reader: &mut dyn io::Read, dir: &Path) -> Result<Config, ConfigLoadError> {
            let config = Config::load_from_reader_inner(reader, path)?;

            // invariant: ordered in the reverse order of precendence: the later element
            // take precedence from the previous one
            let mut included_configs = mem::take(&mut config.include)
                .filter_map(|included_config| included_config.load().transpose())
                .collect::<Result<VecDeque<Config>, ConfigLoadError>>()?;

            let mut i = 0;
            while i < included_configs.len() {
                let mut insert_index = i;
                for included_config in mem::take(&mut included_configs[i].include) {
                    if let Some(loaded_config) = included_config.load()? {
                        included_configs.insert(insert_index, loaded_config);
                        insert_index += 1;
                    }
                }
                
                // If new configs inserted, recursively check if it has included configs
                i = if insert_index > i { i } else { i + 1 };
            }
            
            Ok(config)
        }

        inner(&mut reader, dir)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigLoadError> {
        fn inner(path: &Path) -> Result<Config, ConfigLoadError> {
            match File::open(path) {
                Ok(file) => {
                    let file = FileLock::new_shared(file)?.set_file_path(path);
                    // Any regular file must have a parent dir
                    Config::load_from_reader(file, path.parent().unwrap())
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Default::default()),
                Err(err) => Err(err.into()),
            }
        }

        inner(path.as_ref())
    }

    pub fn get_registry_index(&self, name: &str) -> Option<&str> {
        let registry = self.registries.as_ref()?.get(name)?;

        if let Some(name) = registry.replace_with.as_deref() {
            self.get_registry_index(name)
        } else {
            registry.index.as_deref()
        }
    }

    pub fn get_registry(&self, name: &str) -> Option<&Registry> {
        let registry = self.registries.as_ref()?.get(name)?;

        if let Some(name) = registry.replace_with.as_deref() {
            self.get_registry(name)
        } else {
            Some(registry)
        }
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

impl From<toml_edit::TomlError> for ConfigLoadError {
    fn from(e: toml_edit::TomlError) -> Self {
        ConfigLoadError::TomlParse(Box::new(e.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{io::Cursor, path::MAIN_SEPARATOR};

    use compact_str::format_compact;

    const CONFIG: &str = r#"
[env]
# Set ENV_VAR_NAME=value for any process run by Cargo
ENV_VAR_NAME = "value"
# Set even if already present in environment
ENV_VAR_NAME_2 = { value = "value", force = true }
# Value is relative to .cargo directory containing `config.toml`, make absolute
ENV_VAR_NAME_3 = { value = "relative-path", relative = true }

[http]
debug = false               # HTTP debugging
proxy = "host:port"         # HTTP proxy in libcurl format
timeout = 30                # timeout for each HTTP request, in seconds
cainfo = "cert.pem"         # path to Certificate Authority (CA) bundle

[install]
root = "/some/path"         # `cargo install` destination directory

[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"

[registry]
default = "private-registry"
credential-provider = "cargo:token"
global-credential-providers = ["cargo:token", "cargo:libsecret"]

[credential-alias]
custom = ["cargo-credential-example", "--account", "test"]
    "#;

    #[test]
    fn test_loading() {
        let config = Config::load_from_reader(Cursor::new(&CONFIG), Path::new("root")).unwrap();

        assert_eq!(
            config.install.unwrap().root.as_deref().unwrap(),
            Path::new("/some/path")
        );

        let http = config.http.unwrap();
        assert_eq!(http.proxy.unwrap(), CompactString::const_new("host:port"));
        assert_eq!(http.timeout.unwrap(), 30);
        assert_eq!(http.cainfo.unwrap(), Path::new("root").join("cert.pem"));

        let env = config.env.unwrap();
        assert_eq!(env.len(), 3);
        assert_eq!(
            env.get("ENV_VAR_NAME").unwrap(),
            &Env::Value(CompactString::const_new("value"))
        );
        assert_eq!(
            env.get("ENV_VAR_NAME_2").unwrap(),
            &Env::WithOptions {
                value: CompactString::new("value"),
                force: Some(true),
                relative: None,
            }
        );
        assert_eq!(
            env.get("ENV_VAR_NAME_3").unwrap(),
            &Env::WithOptions {
                value: format_compact!("root{MAIN_SEPARATOR}relative-path"),
                force: None,
                relative: Some(true),
            }
        );

        let registries = config.registries.unwrap();
        let private_registry = registries.get("private-registry").unwrap();
        assert_eq!(
            private_registry.index.as_deref(),
            Some("sparse+https://registry.example.com/index/")
        );
        assert!(matches!(
            private_registry.credential_provider.as_ref(),
            Some(CredentialProvider::String(provider)) if provider == "cargo:token"
        ));

        let registry = config.registry.unwrap();
        assert_eq!(registry.default.as_deref(), Some("private-registry"));
        assert!(matches!(
            registry.credential_provider.as_ref(),
            Some(CredentialProvider::String(provider)) if provider == "cargo:token"
        ));
        assert_eq!(
            registry.global_credential_providers.as_deref(),
            Some(
                &[
                    CompactString::const_new("cargo:token"),
                    CompactString::const_new("cargo:libsecret"),
                ][..]
            )
        );

        let aliases = config.credential_alias.unwrap();
        assert!(matches!(
            aliases.get("custom"),
            Some(CredentialProvider::Array(provider))
                if provider
                    == &[
                        CompactString::const_new("cargo-credential-example"),
                        CompactString::const_new("--account"),
                        CompactString::const_new("test"),
                    ]
        ));
    }
}
