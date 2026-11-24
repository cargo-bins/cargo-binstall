use std::{
    env,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
};

use binstalk::registry::{CredentialProvider, CredentialProviderError, Registry, RegistryAuth};
use binstalk_manifests::{
    cargo_config::{Config as CargoConfig, CredentialProvider as ProviderConfig},
    cargo_credentials::Credentials,
};
use binstalk_types::SecretString;
use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use tracing::debug;
use zeroize::Zeroizing;

/// Built-in providers of cargo, see
/// <https://doc.rust-lang.org/cargo/reference/registry-authentication.html>.
///
/// An alias in `[credential-alias]` cannot shadow them.
const BUILT_IN_PROVIDERS: &[&str] = &[
    "cargo:token",
    "cargo:token-from-stdout",
    "cargo:wincred",
    "cargo:macos-keychain",
    "cargo:libsecret",
];

#[derive(Clone, Debug, Eq, PartialEq)]
enum Provider {
    CargoToken,
    CargoTokenFromStdout(Vec<CompactString>),
    /// A built-in provider of cargo that cargo-binstall does not implement,
    /// e.g. `cargo:libsecret`.
    Unsupported(CompactString),
    /// An external provider speaking cargo's credential provider protocol:
    /// the program followed by its args.
    Process(Vec<CompactString>),
}

fn normalize_registry_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '-' | '_' => '_',
            _ => ch.to_ascii_lowercase(),
        })
        .collect()
}

pub(crate) fn get_registry_env_var(name: &str, suffix: &str) -> Option<String> {
    let normalized_name = normalize_registry_name(name);
    let suffix = format!("_{}", suffix.to_ascii_uppercase());

    env::vars().find_map(|(key, value)| {
        let registry_name = key
            .strip_prefix("CARGO_REGISTRIES_")?
            .strip_suffix(&suffix)?;

        (normalize_registry_name(registry_name) == normalized_name).then_some(value)
    })
}

fn split_provider_string(provider: &str) -> Vec<CompactString> {
    provider
        .split_whitespace()
        .map(CompactString::from)
        .collect()
}

fn provider_config_to_vec(provider: &ProviderConfig) -> Vec<CompactString> {
    match provider {
        ProviderConfig::String(provider) => split_provider_string(provider),
        ProviderConfig::Array(provider) => provider.clone(),
    }
}

/// Like cargo, a provider without args is looked up in `[credential-alias]`,
/// unless it is a built-in provider. The alias is not resolved recursively.
fn resolve_provider(cargo_config: &CargoConfig, provider: Vec<CompactString>) -> Provider {
    let provider = match &provider[..] {
        [name] if !BUILT_IN_PROVIDERS.contains(&name.as_str()) => cargo_config
            .credential_alias
            .get(name.as_str())
            .map(provider_config_to_vec)
            .unwrap_or(provider),
        _ => provider,
    };

    match provider.first().map(CompactString::as_str) {
        Some("cargo:token") => Provider::CargoToken,
        Some("cargo:token-from-stdout") => Provider::CargoTokenFromStdout(provider[1..].to_vec()),
        Some(name) if name.starts_with("cargo:") => Provider::Unsupported(name.into()),
        // An empty provider fails when spawned, like in cargo.
        _ => Provider::Process(provider),
    }
}

/// Return the providers of the registry, in the order they are tried.
fn resolve_registry_credential_providers(
    cargo_config: &CargoConfig,
    registry_name: Option<&str>,
) -> Vec<Provider> {
    if let Some(registry_name) = registry_name {
        if let Some(provider) = get_registry_env_var(registry_name, "CREDENTIAL_PROVIDER") {
            return vec![resolve_provider(
                cargo_config,
                split_provider_string(&provider),
            )];
        }

        if let Some(provider) = cargo_config
            .get_registry(registry_name)
            .and_then(|registry| registry.credential_provider.as_ref())
        {
            return vec![resolve_provider(
                cargo_config,
                provider_config_to_vec(provider),
            )];
        }
    }

    // Later providers take precedence, so they are tried first.
    match cargo_config
        .registry
        .as_ref()
        .and_then(|registry| registry.global_credential_providers.as_ref())
        .filter(|providers| !providers.is_empty())
    {
        Some(providers) => providers
            .iter()
            .rev()
            .map(|provider| resolve_provider(cargo_config, split_provider_string(provider)))
            .collect(),
        None => vec![Provider::CargoToken],
    }
}

fn resolve_cargo_token(
    cargo_home: &Path,
    registry_name: Option<&str>,
) -> Result<Option<SecretString>, CredentialProviderError> {
    let Some(registry_name) = registry_name else {
        return Ok(None);
    };

    if let Some(token) = get_registry_env_var(registry_name, "TOKEN") {
        return Ok(Some(SecretString::from_boxed_str(token.into_boxed_str())));
    }

    Ok(Credentials::load_from_home(cargo_home)?
        .get_registry_token(registry_name)
        .cloned())
}

fn resolve_provider_command_arg(arg: &str, index_url: &str) -> String {
    // Cargo's `BasicProcessCredential` replaces `{index_url}` before spawning `cargo:token-from-stdout` commands.
    // https://github.com/rust-lang/cargo/blob/master/src/cargo/util/credential/adaptor.rs#L27-L34
    arg.replace("{index_url}", index_url)
}

fn resolve_cargo_token_from_stdout(
    provider_args: &[CompactString],
    registry_name: Option<&str>,
    index_url: &str,
) -> io::Result<SecretString> {
    let Some(executable) = provider_args.first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The first argument to `cargo:token-from-stdout` must be a command that prints a token on stdout",
        ));
    };

    let mut command = Command::new(executable.as_str());
    command
        .args(
            provider_args[1..]
                .iter()
                .map(|arg| resolve_provider_command_arg(arg, index_url)),
        )
        .env(
            "CARGO",
            env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
        )
        .env("CARGO_REGISTRY_INDEX_URL", index_url)
        .stdout(Stdio::piped());

    if let Some(name) = registry_name {
        command.env("CARGO_REGISTRY_NAME_OPT", name);
    }

    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().unwrap();

    let mut buffer = SecretString::from_string(String::new());
    stdout.read_to_string(&mut buffer)?;

    if let Some(line) = buffer.lines().next() {
        let end = line.len();
        let line_ending_len = if buffer[end..].starts_with("\r\n") {
            2
        } else if buffer[end..].starts_with('\n') {
            1
        } else {
            0
        };

        if buffer.len() > end + line_ending_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "process `{executable}` returned more than one line of output; expected a single token",
                ),
            ));
        }
        buffer.truncate(end);
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "process `{executable}` failed with status `{status}`",
        )));
    }

    Ok(buffer)
}

/// Version of cargo's credential provider protocol.
const PROTOCOL_VERSION_1: u32 = 1;

#[derive(Deserialize)]
struct CredentialHello {
    v: Vec<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
struct RegistryInfo<'a> {
    index_url: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    headers: &'a [String],
}

/// A `get` request for the `read` operation, the only one cargo-binstall
/// needs.
#[derive(Serialize)]
struct CredentialRequest<'a> {
    v: u32,
    registry: RegistryInfo<'a>,
    kind: &'static str,
    operation: &'static str,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    args: &'a [&'a str],
}

#[derive(Deserialize)]
enum CredentialResponse {
    Ok(GetResponse),
    Err(ErrorResponse),
}

/// `cache` and `operation_independent` are ignored: the token is only used
/// for reading and is kept for the rest of the session.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum GetResponse {
    Get { token: String },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum ErrorResponse {
    UrlNotSupported,
    NotFound,
    OperationNotSupported,
    #[serde(rename_all = "kebab-case")]
    Other {
        message: String,
        #[serde(default)]
        caused_by: Vec<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug)]
enum ProcessOutcome {
    Token(SecretString),
    NotFound,
    UrlNotSupported,
}

fn protocol_error(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn talk_to_credential_process(
    child: &mut Child,
    program: &str,
    registry: RegistryInfo<'_>,
    args: &[&str],
) -> io::Result<ProcessOutcome> {
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stdin = child.stdin.take().unwrap();
    let mut line = Zeroizing::new(String::new());

    if stdout.read_line(&mut line)? == 0 {
        return Err(protocol_error("exited without sending its hello"));
    }
    let hello: CredentialHello = serde_json::from_str(&line)
        .map_err(|err| protocol_error(format_args!("failed to parse its hello: {err}")))?;
    if !hello.v.contains(&PROTOCOL_VERSION_1) {
        return Err(protocol_error(format_args!(
            "it supports protocol versions {:?}, while cargo-binstall supports [{PROTOCOL_VERSION_1}]",
            hello.v
        )));
    }

    let request = serde_json::to_string(&CredentialRequest {
        v: PROTOCOL_VERSION_1,
        registry,
        kind: "get",
        operation: "read",
        args,
    })
    .map_err(io::Error::other)?;
    debug!("credential provider `{program}` < {request}");
    writeln!(stdin, "{request}")?;

    line.clear();
    if stdout.read_line(&mut line)? == 0 {
        return Err(protocol_error("exited without sending a response"));
    }
    // Do not print the error of serde_json, as it might include the token.
    let response: CredentialResponse =
        serde_json::from_str(&line).map_err(|_| protocol_error("failed to parse its response"))?;

    // Closing stdin tells the provider that we are done.
    drop(stdin);
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!("failed with status `{status}`")));
    }

    match response {
        CredentialResponse::Ok(GetResponse::Get { token }) => {
            debug!("credential provider `{program}` returned a token");
            Ok(ProcessOutcome::Token(SecretString::from_string(token)))
        }
        CredentialResponse::Err(err) => match err {
            ErrorResponse::UrlNotSupported => Ok(ProcessOutcome::UrlNotSupported),
            ErrorResponse::NotFound => Ok(ProcessOutcome::NotFound),
            ErrorResponse::OperationNotSupported => {
                Err(io::Error::other("the operation is not supported"))
            }
            ErrorResponse::Other { message, caused_by } => Err(io::Error::other(
                caused_by
                    .iter()
                    .fold(message, |message, cause| format!("{message}: {cause}")),
            )),
            ErrorResponse::Unknown => Err(io::Error::other("unknown error")),
        },
    }
}

/// Run an external provider with cargo's credential provider protocol:
/// <https://doc.rust-lang.org/cargo/reference/credential-provider-protocol.html>
fn run_credential_process(
    provider: &[CompactString],
    registry: RegistryInfo<'_>,
) -> io::Result<ProcessOutcome> {
    let Some((program, args)) = provider.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "credential provider is empty",
        ));
    };
    let args: Vec<&str> = args.iter().map(CompactString::as_str).collect();
    let with_context = |err: io::Error| {
        io::Error::new(
            err.kind(),
            format!("credential provider `{program}`: {err}"),
        )
    };

    // The provider inherits stderr, so that it can prompt the user.
    let mut child = Command::new(program.as_str())
        .arg("--cargo-plugin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(with_context)?;

    let outcome = talk_to_credential_process(&mut child, program, registry, &args);
    if outcome.is_err() {
        // Make sure the provider is stopped.
        let _ = child.kill();
        let _ = child.wait();
    }
    outcome.map_err(with_context)
}

/// Cargo's credential providers configured for a registry.
#[derive(Debug)]
struct CargoCredentialProviders {
    providers: Vec<Provider>,
    registry_name: Option<CompactString>,
    index_url: String,
    cargo_home: PathBuf,
}

impl CredentialProvider for CargoCredentialProviders {
    fn get_token(
        &self,
        headers: &[String],
    ) -> Result<Option<SecretString>, CredentialProviderError> {
        let registry_name = self.registry_name.as_deref();
        let mut any_not_found = false;
        let mut unsupported = Vec::new();

        // Like cargo, try the next provider if one does not have the token
        // or does not support the registry.
        for provider in &self.providers {
            debug!(?provider, registry_name, "Getting registry token");

            match provider {
                Provider::CargoToken => match resolve_cargo_token(&self.cargo_home, registry_name)?
                {
                    Some(token) => return Ok(Some(token)),
                    None => any_not_found = true,
                },
                Provider::CargoTokenFromStdout(args) => {
                    return Ok(Some(resolve_cargo_token_from_stdout(
                        args,
                        registry_name,
                        &self.index_url,
                    )?));
                }
                Provider::Process(provider) => {
                    let registry = RegistryInfo {
                        index_url: &self.index_url,
                        name: registry_name,
                        headers,
                    };
                    match run_credential_process(provider, registry)? {
                        ProcessOutcome::Token(token) => return Ok(Some(token)),
                        ProcessOutcome::NotFound => any_not_found = true,
                        ProcessOutcome::UrlNotSupported => (),
                    }
                }
                Provider::Unsupported(name) => {
                    debug!("cargo-binstall does not support credential provider `{name}`");
                    unsupported.push(name.as_str());
                }
            }
        }

        if any_not_found {
            Ok(None)
        } else if unsupported.is_empty() {
            Err("no credential provider could handle the request".into())
        } else {
            Err(format!(
                "cargo-binstall does not support credential provider {}",
                unsupported
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
            .into())
        }
    }
}

/// Return the auth for the registry. Its credential providers only run
/// once the registry asks for a token, like in cargo.
pub(crate) fn resolve_registry_auth(
    cargo_config: &CargoConfig,
    cargo_home: &Path,
    registry_name: Option<&str>,
    registry: &Registry,
) -> RegistryAuth {
    let providers = resolve_registry_credential_providers(cargo_config, registry_name);
    debug!(
        ?providers,
        registry_name, "Resolved registry credential providers"
    );

    RegistryAuth::with_provider(
        registry_name.map(CompactString::from),
        Arc::new(CargoCredentialProviders {
            providers,
            registry_name: registry_name.map(CompactString::from),
            index_url: registry.cargo_install_index_arg(),
            cargo_home: cargo_home.to_owned(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::{env, io::Cursor, sync::Mutex};

    use super::*;
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    const INDEX: &str = "sparse+https://registry.example.com/index/";

    fn config(toml: &str) -> CargoConfig {
        CargoConfig::load_from_reader(Cursor::new(toml), Path::new(".")).unwrap()
    }

    fn providers(toml: &str) -> Vec<Provider> {
        resolve_registry_credential_providers(&config(toml), Some("private-registry"))
    }

    fn process(args: &[&str]) -> Provider {
        Provider::Process(args.iter().copied().map(CompactString::from).collect())
    }

    fn get_token(
        toml: &str,
        cargo_home: &Path,
    ) -> Result<Option<SecretString>, CredentialProviderError> {
        CargoCredentialProviders {
            providers: providers(toml),
            registry_name: Some("private-registry".into()),
            index_url: INDEX.into(),
            cargo_home: cargo_home.to_owned(),
        }
        .get_token(&[])
    }

    /// Set an env var for the lifetime of the guard.
    struct EnvVar(&'static str);

    impl EnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: tests are run with cargo-nextest (one process per test), so no other
            // thread reads or writes the environment concurrently.
            unsafe { env::set_var(key, value) };
            Self(key)
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            // SAFETY: see `EnvVar::set`
            unsafe { env::remove_var(self.0) };
        }
    }

    #[test]
    fn test_get_registry_env_var_normalizes_registry_name() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = EnvVar::set("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN", "token");

        assert_eq!(
            get_registry_env_var("private-registry", "TOKEN").as_deref(),
            Some("token")
        );
    }

    #[test]
    fn test_global_provider_defaults_to_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(providers(""), [Provider::CargoToken]);
        assert_eq!(
            providers("[registry]\nglobal-credential-providers = []"),
            [Provider::CargoToken]
        );
    }

    #[test]
    fn test_registry_provider_env_takes_precedence_over_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = EnvVar::set(
            "CARGO_REGISTRIES_PRIVATE_REGISTRY_CREDENTIAL_PROVIDER",
            "cargo:libsecret",
        );

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"
                "#
            ),
            [Provider::Unsupported("cargo:libsecret".into())]
        );
    }

    #[test]
    fn test_registry_provider_alias_enables_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "custom"

[credential-alias]
custom = "cargo:token"
                "#
            ),
            [Provider::CargoToken]
        );
    }

    #[test]
    fn test_registry_provider_env_alias_enables_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = EnvVar::set(
            "CARGO_REGISTRIES_PRIVATE_REGISTRY_CREDENTIAL_PROVIDER",
            "custom",
        );

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:libsecret"

[credential-alias]
custom = "cargo:token"
                "#
            ),
            [Provider::CargoToken]
        );
    }

    #[test]
    fn test_registry_provider_alias_to_process() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "custom"

[credential-alias]
custom = ["cargo-credential-example", "--account", "test"]
                "#
            ),
            [process(&["cargo-credential-example", "--account", "test"])]
        );
    }

    #[test]
    fn test_registry_provider_array_alias_enables_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = ["custom"]

[credential-alias]
custom = "cargo:token"
                "#
            ),
            [Provider::CargoToken]
        );
    }

    #[test]
    fn test_registry_provider_alias_needs_no_args() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "custom --arg"

[credential-alias]
custom = "cargo:token"
                "#
            ),
            [process(&["custom", "--arg"])]
        );
    }

    #[test]
    fn test_registry_provider_builtin_cargo_names_do_not_use_aliases() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token-from-stdout"

[credential-alias]
"cargo:token-from-stdout" = "cargo:token"
                "#
            ),
            [Provider::CargoTokenFromStdout(Vec::new())]
        );
    }

    #[test]
    fn test_registry_provider_string_enables_cargo_token_from_stdout() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token-from-stdout rustc --print sysroot"
                "#
            ),
            [Provider::CargoTokenFromStdout(vec![
                "rustc".into(),
                "--print".into(),
                "sysroot".into()
            ])]
        );
    }

    #[test]
    fn test_registry_provider_alias_enables_cargo_token_from_stdout() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(
            providers(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "custom"

[credential-alias]
custom = ["cargo:token-from-stdout", "rustc", "--print", "sysroot"]
                "#
            ),
            [Provider::CargoTokenFromStdout(vec![
                "rustc".into(),
                "--print".into(),
                "sysroot".into()
            ])]
        );
    }

    #[test]
    fn test_global_providers_are_tried_last_first() {
        let _guard = ENV_LOCK.lock().unwrap();

        assert_eq!(
            providers(
                r#"
[registry]
global-credential-providers = [
    "cargo:token",
    "cargo:libsecret",
    "cargo-credential-example --account test",
    "custom",
]

[credential-alias]
custom = ["cargo:token-from-stdout", "rustc", "--print", "sysroot"]
                "#
            ),
            [
                Provider::CargoTokenFromStdout(vec![
                    "rustc".into(),
                    "--print".into(),
                    "sysroot".into()
                ]),
                process(&["cargo-credential-example", "--account", "test"]),
                Provider::Unsupported("cargo:libsecret".into()),
                Provider::CargoToken,
            ]
        );
    }

    #[test]
    fn test_get_token_uses_token_from_stdout_provider() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();

        let token = get_token(
            r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token-from-stdout rustc --print sysroot"
            "#,
            tempdir.path(),
        )
        .unwrap()
        .unwrap();

        assert!(!token.is_empty());
    }

    #[test]
    fn test_get_token_uses_credentials_when_cargo_token_is_enabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();
        std::fs::write(
            tempdir.path().join("credentials.toml"),
            r#"
[registries.private-registry]
token = "secret-token"
            "#,
        )
        .unwrap();

        let token = get_token(
            r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"
            "#,
            tempdir.path(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(&token[..], "secret-token");
    }

    #[test]
    fn test_get_token_uses_env_token_when_cargo_token_is_enabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = EnvVar::set("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN", "secret-token");
        let tempdir = tempdir().unwrap();

        let token = get_token(
            r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"
            "#,
            tempdir.path(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(&token[..], "secret-token");
    }

    #[test]
    fn test_get_token_ignores_config_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();

        let token = get_token(
            r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"
token = "secret-token"
            "#,
            tempdir.path(),
        )
        .unwrap();

        assert!(token.is_none());
    }

    #[test]
    fn test_get_token_fails_when_provider_is_not_supported() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _env = EnvVar::set("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN", "secret-token");
        let tempdir = tempdir().unwrap();

        let err = get_token(
            r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:libsecret"
            "#,
            tempdir.path(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("`cargo:libsecret`"), "{err}");
    }

    #[test]
    fn test_resolve_registry_auth_does_not_run_providers() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();
        let registry: Registry = INDEX.parse().unwrap();

        let auth = resolve_registry_auth(
            &config(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "does-not-exist"
                "#,
            ),
            tempdir.path(),
            Some("private-registry"),
            &registry,
        );

        assert_eq!(auth.registry_name(), Some("private-registry"));
    }

    /// Tests with a fake provider speaking cargo's credential provider
    /// protocol, written as a shell script.
    #[cfg(unix)]
    mod process {
        use std::{fs, os::unix::fs::PermissionsExt};

        use super::*;

        /// Write a provider that records the args and the request in
        /// `dir`, and responds with `response`.
        fn fake_provider(dir: &Path, name: &str, response: &str) -> PathBuf {
            let path = dir.join(name);
            fs::write(
                &path,
                format!(
                    r#"#!/bin/sh
printf '%s\n' "$@" > '{dir}/{name}.args'
echo '{{"v":[1]}}'
read -r request
printf '%s\n' "$request" > '{dir}/{name}.request'
echo '{response}'
# Cargo closes stdin once it has read the response.
read -r _ || exit 0
exit 1
"#,
                    dir = dir.display(),
                ),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn read_request(dir: &Path, name: &str) -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(dir.join(format!("{name}.request"))).unwrap())
                .unwrap()
        }

        const TOKEN_RESPONSE: &str = r#"{"Ok":{"kind":"get","token":"process-token","cache":"session","operation_independent":true}}"#;

        #[test]
        fn test_get_token_from_process() {
            let _guard = ENV_LOCK.lock().unwrap();
            let tempdir = tempdir().unwrap();
            let provider = fake_provider(tempdir.path(), "provider", TOKEN_RESPONSE);

            let token = get_token(
                &format!(
                    r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = ["{}", "--account", "test"]
                    "#,
                    provider.display()
                ),
                tempdir.path(),
            )
            .unwrap()
            .unwrap();

            assert_eq!(&token[..], "process-token");
            assert_eq!(
                fs::read_to_string(tempdir.path().join("provider.args")).unwrap(),
                "--cargo-plugin\n"
            );
            assert_eq!(
                read_request(tempdir.path(), "provider"),
                serde_json::json!({
                    "v": 1,
                    "registry": {
                        "index-url": INDEX,
                        "name": "private-registry",
                    },
                    "kind": "get",
                    "operation": "read",
                    "args": ["--account", "test"],
                })
            );
        }

        #[test]
        fn test_get_token_passes_headers_to_process() {
            let _guard = ENV_LOCK.lock().unwrap();
            let tempdir = tempdir().unwrap();
            let provider = fake_provider(tempdir.path(), "provider", TOKEN_RESPONSE);

            let token = CargoCredentialProviders {
                providers: vec![process(&[provider.to_str().unwrap()])],
                registry_name: None,
                index_url: INDEX.into(),
                cargo_home: tempdir.path().to_owned(),
            }
            .get_token(&["www-authenticate: Cargo login_url=\"https://example.com\"".into()])
            .unwrap()
            .unwrap();

            assert_eq!(&token[..], "process-token");
            assert_eq!(
                read_request(tempdir.path(), "provider"),
                serde_json::json!({
                    "v": 1,
                    "registry": {
                        "index-url": INDEX,
                        "headers": ["www-authenticate: Cargo login_url=\"https://example.com\""],
                    },
                    "kind": "get",
                    "operation": "read",
                })
            );
        }

        #[test]
        fn test_get_token_tries_next_global_provider() {
            let _guard = ENV_LOCK.lock().unwrap();
            let _env = EnvVar::set("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN", "secret-token");
            let tempdir = tempdir().unwrap();
            let not_found = fake_provider(
                tempdir.path(),
                "not-found",
                r#"{"Err":{"kind":"not-found"}}"#,
            );
            let url_not_supported = fake_provider(
                tempdir.path(),
                "url-not-supported",
                r#"{"Err":{"kind":"url-not-supported"}}"#,
            );

            let token = get_token(
                &format!(
                    r#"
[registry]
global-credential-providers = ["cargo:token", "{}", "{}"]
                    "#,
                    not_found.display(),
                    url_not_supported.display()
                ),
                tempdir.path(),
            )
            .unwrap()
            .unwrap();

            assert_eq!(&token[..], "secret-token");
            read_request(tempdir.path(), "not-found");
            read_request(tempdir.path(), "url-not-supported");
        }

        #[test]
        fn test_get_token_returns_none_when_process_has_no_token() {
            let _guard = ENV_LOCK.lock().unwrap();
            let tempdir = tempdir().unwrap();
            let provider = fake_provider(
                tempdir.path(),
                "provider",
                r#"{"Err":{"kind":"not-found"}}"#,
            );

            let token = get_token(
                &format!(
                    "[registries.private-registry]\ncredential-provider = '{}'",
                    provider.display()
                ),
                tempdir.path(),
            )
            .unwrap();

            assert!(token.is_none());
        }

        #[test]
        fn test_get_token_fails_when_no_process_supports_url() {
            let _guard = ENV_LOCK.lock().unwrap();
            let tempdir = tempdir().unwrap();
            let provider = fake_provider(
                tempdir.path(),
                "provider",
                r#"{"Err":{"kind":"url-not-supported"}}"#,
            );

            let err = get_token(
                &format!(
                    "[registries.private-registry]\ncredential-provider = '{}'",
                    provider.display()
                ),
                tempdir.path(),
            )
            .unwrap_err();

            assert_eq!(
                err.to_string(),
                "no credential provider could handle the request"
            );
        }

        #[test]
        fn test_get_token_reports_process_error() {
            let _guard = ENV_LOCK.lock().unwrap();
            let tempdir = tempdir().unwrap();
            let provider = fake_provider(
                tempdir.path(),
                "provider",
                r#"{"Err":{"kind":"other","message":"vault is locked","caused-by":["timed out"]}}"#,
            );

            let err = get_token(
                &format!(
                    "[registries.private-registry]\ncredential-provider = '{}'",
                    provider.display()
                ),
                tempdir.path(),
            )
            .unwrap_err();

            let err = err.to_string();
            assert!(err.contains("vault is locked: timed out"), "{err}");
            assert!(err.contains("provider"), "{err}");
        }

        #[test]
        fn test_get_token_rejects_unsupported_protocol_version() {
            let _guard = ENV_LOCK.lock().unwrap();
            let tempdir = tempdir().unwrap();
            let provider = tempdir.path().join("provider");
            fs::write(&provider, "#!/bin/sh\necho '{\"v\":[2]}'\nsleep 10\n").unwrap();
            fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();

            let err = get_token(
                &format!(
                    "[registries.private-registry]\ncredential-provider = '{}'",
                    provider.display()
                ),
                tempdir.path(),
            )
            .unwrap_err();

            assert!(err.to_string().contains("protocol versions [2]"), "{err}");
        }

        #[test]
        fn test_get_token_fails_when_process_exits_early() {
            let _guard = ENV_LOCK.lock().unwrap();
            let tempdir = tempdir().unwrap();
            let provider = tempdir.path().join("provider");
            fs::write(&provider, "#!/bin/sh\nexit 3\n").unwrap();
            fs::set_permissions(&provider, fs::Permissions::from_mode(0o755)).unwrap();

            let err = get_token(
                &format!(
                    "[registries.private-registry]\ncredential-provider = '{}'",
                    provider.display()
                ),
                tempdir.path(),
            )
            .unwrap_err();

            assert!(
                err.to_string().contains("without sending its hello"),
                "{err}"
            );
        }
    }
}
