//! Cargo's `.cargo/config.toml`
//!
//! This manifest is used by Cargo to load configurations stored by users.
//!
//! Binstall reads from them to be compatible with `cargo-install`'s behavior.

use std::{
    collections::{BTreeMap, HashSet, VecDeque, btree_map::Entry as BTreeMapEntry},
    fs::File,
    io, mem,
    path::{Path, PathBuf},
};

use compact_str::CompactString;
use fs_lock::FileLock;
use home::cargo_home;
use miette::Diagnostic;
use normalize_path::NormalizePath;
use serde::Deserialize;
use thiserror::Error;

/// Merge a lower-precedence config into `self`.
///
/// Values already present in `self` always win; `other` only fills in what is
/// missing. Impls destructure `other` exhaustively so adding a field without
/// deciding how it merges is a compile error.
trait Merge {
    fn merge(&mut self, other: Self);
}

fn overwrite_none<T>(left: &mut Option<T>, right: Option<T>) {
    if left.is_none() {
        *left = right;
    }
}

fn merge_option<T: Merge>(left: &mut Option<T>, right: Option<T>) {
    match (left.as_mut(), right) {
        (None, right) => *left = right,
        (Some(_), None) => (),
        (Some(left), Some(right)) => left.merge(right),
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Install {
    /// `cargo install` destination directory
    pub root: Option<PathBuf>,
}

impl Merge for Install {
    fn merge(&mut self, other: Self) {
        let Self { root } = other;
        overwrite_none(&mut self.root, root);
    }
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

impl Merge for Http {
    fn merge(&mut self, other: Self) {
        let Self {
            proxy,
            timeout,
            cainfo,
        } = other;
        overwrite_none(&mut self.proxy, proxy);
        overwrite_none(&mut self.timeout, timeout);
        overwrite_none(&mut self.cainfo, cainfo);
    }
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

impl Merge for Registry {
    fn merge(&mut self, other: Self) {
        let Self {
            index,
            replace_with,
            credential_provider,
        } = other;
        overwrite_none(&mut self.index, index);
        overwrite_none(&mut self.replace_with, replace_with);
        overwrite_none(&mut self.credential_provider, credential_provider);
    }
}

type GlobalCredentialProviders = Option<VecDeque<CompactString>>;
fn merge_global_credential_providers(
    left: &mut GlobalCredentialProviders,
    right: GlobalCredentialProviders,
) {
    match (left.as_mut(), right) {
        (None, right) => *left = right,
        (Some(_), None) => (),
        (Some(left), Some(right)) => {
            left.reserve(right.len());
            for provider in right.into_iter().rev() {
                left.push_front(provider);
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct DefaultRegistry {
    pub default: Option<CompactString>,
    #[serde(rename = "credential-provider")]
    pub credential_provider: Option<CredentialProvider>,
    #[serde(rename = "global-credential-providers")]
    pub global_credential_providers: GlobalCredentialProviders,
}

impl Merge for DefaultRegistry {
    fn merge(&mut self, other: Self) {
        let Self {
            default,
            credential_provider,
            global_credential_providers,
        } = other;
        overwrite_none(&mut self.default, default);
        overwrite_none(&mut self.credential_provider, credential_provider);
        merge_global_credential_providers(
            &mut self.global_credential_providers,
            global_credential_providers,
        );
    }
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

    pub fn path_mut(&mut self) -> &mut PathBuf {
        match self {
            Self::Path(path) => path,
            Self::Extended { path, .. } => path,
        }
    }

    pub fn optional(&self) -> bool {
        match self {
            Self::Path(..) => false,
            Self::Extended { optional, .. } => *optional,
        }
    }

    fn open(&self) -> io::Result<Option<FileLock>> {
        let path = self.path().canonicalize()?;

        match File::open(&path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound && self.optional() => Ok(None),
            res => Ok(Some(FileLock::new_shared(res?)?.set_file_path(path))),
        }
    }
}

fn merge_btreemap<K: Ord, V>(left: &mut BTreeMap<K, V>, right: BTreeMap<K, V>) {
    for (k, v) in right.into_iter() {
        left.entry(k).or_insert(v);
    }
}

fn merge_btreemap_recursive<K: Ord, V: Merge>(left: &mut BTreeMap<K, V>, right: BTreeMap<K, V>) {
    for (k, v) in right.into_iter() {
        match left.entry(k) {
            BTreeMapEntry::Vacant(entry) => {
                entry.insert(v);
            }
            BTreeMapEntry::Occupied(entry) => entry.into_mut().merge(v),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[non_exhaustive]
pub struct Config {
    pub install: Option<Install>,
    pub http: Option<Http>,
    #[serde(default)]
    pub env: BTreeMap<CompactString, Env>,
    #[serde(default)]
    pub registries: BTreeMap<CompactString, Registry>,
    pub registry: Option<DefaultRegistry>,
    #[serde(default)]
    pub include: Vec<IncludedConfig>,
    #[serde(default, rename = "credential-alias")]
    pub credential_alias: BTreeMap<CompactString, CredentialProvider>,
}

impl Config {
    /// Merge `other` into `self`; values already in `self` take precedence.
    ///
    /// `other.include` is ignored: includes are resolved by the loader.
    pub fn merge(&mut self, other: Self) {
        let Self {
            install,
            http,
            env,
            registries,
            registry,
            include: _,
            credential_alias,
        } = other;
        merge_option(&mut self.install, install);
        merge_option(&mut self.http, http);
        merge_btreemap(&mut self.env, env);
        merge_btreemap_recursive(&mut self.registries, registries);
        merge_option(&mut self.registry, registry);
        merge_btreemap(&mut self.credential_alias, credential_alias);
    }
}

fn join_if_relative(path: Option<&mut PathBuf>, dir: &Path) {
    match path {
        Some(path) if path.is_relative() => *path = dir.join(&*path),
        _ => (),
    }
}

fn iterate_reverse_preorder(
    mut stack: Vec<IncludedConfig>,
    mut load_config: impl FnMut(
        &mut dyn io::Read,
        &Path,
    ) -> Result<Vec<IncludedConfig>, ConfigLoadError>,
) -> Result<(), ConfigLoadError> {
    // stack invariant: higher precedence config is the first out
    let mut visited_path = HashSet::new();

    while let Some(config_path) = stack.pop() {
        let Some(file) = config_path.open()? else {
            continue;
        };

        let path = file.get_file_path().unwrap(); // canonicalized path
        let parent = config_path.path().parent().unwrap(); // original path

        // Use the absolute, canonicalized path (expanded symlink) to track the
        // content of the config being loaded, and use the normalized parent
        // (not absolute or expanded symlink) to track the parent, as that
        // would decide all the relative path in the config.
        //
        // Even if one config file has two symlinks, the content might be different
        // if relative path is present.
        if !visited_path.insert((path.to_owned(), parent.normalize())) {
            return Err(ConfigLoadError::DeadLoopInLoading {
                path: path.into(),
                parent: parent.into(),
            });
        }

        stack.extend(load_config(&mut (&file), parent)?);
    }

    Ok(())
}

impl Config {
    pub fn default_path() -> Result<PathBuf, ConfigLoadError> {
        Ok(cargo_home()?.join("config.toml"))
    }

    pub fn load() -> Result<Self, ConfigLoadError> {
        Self::load_from_path(Self::default_path()?)
    }

    fn load_from_reader_inner(
        reader: &mut dyn io::Read,
        dir: &Path,
    ) -> Result<Self, ConfigLoadError> {
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
            for env in config.env.values_mut() {
                let Env::WithOptions {
                    value,
                    relative: Some(true),
                    ..
                } = env
                else {
                    continue;
                };
                let path = Path::new(&value);
                if path.is_relative() {
                    *value = dir.join(path).to_string_lossy().into();
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
            let mut root_config = Config::load_from_reader_inner(reader, dir)?;

            iterate_reverse_preorder(mem::take(&mut root_config.include), |file, parent| {
                let mut config = Config::load_from_reader_inner(file, parent)?;
                let includes = mem::take(&mut config.include);
                root_config.merge(config);
                Ok(includes)
            })?;

            Ok(root_config)
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
        let registry = self.registries.get(name)?;

        if let Some(name) = registry.replace_with.as_deref() {
            self.get_registry_index(name)
        } else {
            registry.index.as_deref()
        }
    }

    pub fn get_registry(&self, name: &str) -> Option<&Registry> {
        let registry = self.registries.get(name)?;

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

    #[error("Detect deadloop in toml at `{path}` with parent `{parent}`")]
    DeadLoopInLoading { path: Box<Path>, parent: Box<Path> },
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

    use compact_str::{ToCompactString, format_compact};

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

        let env = config.env;
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

        let registries = config.registries;
        let private_registry = registries.get("private-registry").unwrap();
        assert_eq!(
            private_registry.index.as_deref(),
            Some("sparse+https://registry.example.com/index/")
        );
        assert!(matches!(
            private_registry.credential_provider.as_ref(),
            Some(CredentialProvider::String(provider)) if provider == "cargo:token"
        ));

        let mut registry = config.registry.unwrap();
        assert_eq!(registry.default.as_deref(), Some("private-registry"));
        assert!(matches!(
            registry.credential_provider.as_ref(),
            Some(CredentialProvider::String(provider)) if provider == "cargo:token"
        ));
        assert_eq!(
            registry
                .global_credential_providers
                .as_mut()
                .map(VecDeque::make_contiguous)
                .as_deref(),
            Some(
                &[
                    CompactString::const_new("cargo:token"),
                    CompactString::const_new("cargo:libsecret"),
                ][..]
            )
        );

        let aliases = config.credential_alias;
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

    #[test]
    fn test_merge_config() {
        let mut config = Config {
            // Covered by test_merge_install_http_include
            install: None,
            http: None,
            include: Vec::new(),
            // Same strategy as env
            credential_alias: BTreeMap::new(),

            env: BTreeMap::from([
                (CompactString::new("1"), Env::Value(CompactString::new("1"))),
                (CompactString::new("2"), Env::Value(CompactString::new("2"))),
            ]),
            registries: BTreeMap::from([
                (
                    CompactString::new("1"),
                    Registry {
                        index: None,
                        replace_with: None,
                        credential_provider: None,
                    },
                ),
                (
                    CompactString::new("2"),
                    Registry {
                        index: Some(CompactString::new("!")),
                        replace_with: None,
                        credential_provider: None,
                    },
                ),
            ]),
            registry: Some(DefaultRegistry {
                default: Some(CompactString::new("1")),
                credential_provider: None,
                global_credential_providers: Some(VecDeque::from([CompactString::new("left")])),
            }),
        };
        config.merge(Config {
            install: None,
            http: None,
            include: Vec::new(),
            credential_alias: BTreeMap::new(),

            env: BTreeMap::from([
                (
                    CompactString::new("2"),
                    Env::Value(CompactString::new("qwewrd")),
                ),
                (CompactString::new("3"), Env::Value(CompactString::new("3"))),
            ]),
            registries: BTreeMap::from([
                (
                    CompactString::new("2"),
                    Registry {
                        index: Some(CompactString::new("indexex")),
                        replace_with: Some(CompactString::new("ere")),
                        credential_provider: None,
                    },
                ),
                (
                    CompactString::new("3"),
                    Registry {
                        index: None,
                        replace_with: Some(CompactString::new("re")),
                        credential_provider: Some(CredentialProvider::String(CompactString::new(
                            "213",
                        ))),
                    },
                ),
            ]),
            registry: Some(DefaultRegistry {
                default: Some(CompactString::new("www1")),
                credential_provider: Some(CredentialProvider::String(CompactString::new("ww213"))),
                global_credential_providers: Some(
                    ["right", "2"].into_iter().map(Into::into).collect(),
                ),
            }),
        });

        assert_eq!(
            config.env,
            [1, 2, 3]
                .iter()
                .map(ToCompactString::to_compact_string)
                .map(|s| (s.clone(), Env::Value(s)))
                .collect::<BTreeMap<_, _>>(),
        );

        assert_eq!(
            config.registries.keys().collect::<Vec<_>>(),
            ["1", "2", "3"]
        );

        let registry_1 = config.registries.remove("1").unwrap();
        assert_eq!(registry_1.index, None);
        assert_eq!(registry_1.replace_with, None);
        assert!(registry_1.credential_provider.is_none());

        let registry_2 = config.registries.remove("2").unwrap();
        assert_eq!(registry_2.index, Some(CompactString::new("!")));
        assert_eq!(registry_2.replace_with, Some(CompactString::new("ere")));
        assert!(registry_2.credential_provider.is_none());

        let registry_3 = config.registries.remove("3").unwrap();
        assert_eq!(registry_3.index, None);
        assert_eq!(registry_3.replace_with, Some(CompactString::new("re")));
        assert!(
            matches!(registry_3.credential_provider.unwrap(), CredentialProvider::String(v) if v == "213")
        );

        let default_registry = config.registry.unwrap();
        assert_eq!(default_registry.default, Some(CompactString::new("1")));
        assert!(
            matches!(default_registry.credential_provider.unwrap(), CredentialProvider::String(v) if v == "ww213")
        );
        assert_eq!(
            default_registry.global_credential_providers.unwrap(),
            ["right", "2", "left"]
                .into_iter()
                .map(CompactString::new)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn test_merge_install_http_include() {
        let mut config = Config {
            install: None,
            http: Some(Http {
                proxy: Some(CompactString::new("left:1")),
                timeout: None,
                cainfo: None,
            }),
            include: vec![IncludedConfig::Path(PathBuf::from("left.toml"))],
            ..Default::default()
        };
        config.merge(Config {
            install: Some(Install {
                root: Some(PathBuf::from("/right")),
            }),
            http: Some(Http {
                proxy: Some(CompactString::new("right:2")),
                timeout: Some(7),
                cainfo: Some(PathBuf::from("right.pem")),
            }),
            include: vec![IncludedConfig::Path(PathBuf::from("right.toml"))],
            ..Default::default()
        });

        // `None` on the left is filled in wholesale.
        assert_eq!(
            config.install.unwrap().root.as_deref(),
            Some(Path::new("/right"))
        );

        // `Some` on both sides merges field by field, left winning.
        let http = config.http.unwrap();
        assert_eq!(http.proxy.as_deref(), Some("left:1"));
        assert_eq!(http.timeout, Some(7));
        assert_eq!(http.cainfo.as_deref(), Some(Path::new("right.pem")));

        // `include` is never merged.
        assert_eq!(
            config
                .include
                .iter()
                .map(IncludedConfig::path)
                .collect::<Vec<_>>(),
            [Path::new("left.toml")]
        );
    }
}
