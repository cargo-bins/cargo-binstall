use std::{
    env, io,
    path::Path,
    process::{Command, Stdio},
};

use binstalk::registry::{Registry, RegistryAuth};
use binstalk_manifests::{
    cargo_config::{Config as CargoConfig, CredentialProvider},
    cargo_credentials::Credentials,
};
use binstalk_types::SecretString;
use compact_str::CompactString;

#[derive(Clone, Debug, Eq, PartialEq)]
enum SupportedRegistryCredentialProvider {
    CargoToken,
    CargoTokenFromStdout(Vec<CompactString>),
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

fn split_provider_string(provider: &str) -> Vec<CompactString> {
    provider
        .split_whitespace()
        .map(CompactString::from)
        .collect()
}

fn resolve_supported_provider_name(
    cargo_config: &CargoConfig,
    provider_name: &str,
    seen_aliases: &mut Vec<CompactString>,
) -> Option<SupportedRegistryCredentialProvider> {
    let provider = split_provider_string(provider_name);
    resolve_supported_provider(cargo_config, &provider, seen_aliases)
}

fn resolve_supported_provider(
    cargo_config: &CargoConfig,
    provider: &[CompactString],
    seen_aliases: &mut Vec<CompactString>,
) -> Option<SupportedRegistryCredentialProvider> {
    let provider_name = provider.first()?;

    if provider_name == "cargo:token" {
        return (provider.len() == 1).then_some(SupportedRegistryCredentialProvider::CargoToken);
    }

    if provider_name == "cargo:token-from-stdout" {
        return (provider.len() > 1).then_some(
            SupportedRegistryCredentialProvider::CargoTokenFromStdout(provider[1..].to_vec()),
        );
    }

    if provider_name.starts_with("cargo:") {
        return None;
    }

    if seen_aliases.iter().any(|alias| alias == provider_name) {
        return None;
    }

    if provider.len() != 1 {
        return None;
    }

    let provider = cargo_config.credential_alias.get(provider_name.as_str())?;

    seen_aliases.push(provider_name.clone());
    let supports = resolve_supported_provider_from_config(cargo_config, provider, seen_aliases);
    seen_aliases.pop();
    supports
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

fn resolve_supported_provider_from_config(
    cargo_config: &CargoConfig,
    provider: &CredentialProvider,
    seen_aliases: &mut Vec<CompactString>,
) -> Option<SupportedRegistryCredentialProvider> {
    match provider {
        CredentialProvider::String(provider) => {
            resolve_supported_provider_name(cargo_config, provider, seen_aliases)
        }
        CredentialProvider::Array(provider) => {
            resolve_supported_provider(cargo_config, provider, seen_aliases)
        }
    }
}

fn resolve_global_supported_provider<'a>(
    cargo_config: &CargoConfig,
    providers: impl DoubleEndedIterator<Item = &'a CompactString>,
) -> Option<SupportedRegistryCredentialProvider> {
    let mut seen_aliases = Vec::new();
    debug_assert!(seen_aliases.is_empty());
    providers.rev().find_map(|provider| {
        resolve_supported_provider_name(cargo_config, provider, &mut seen_aliases)
    })
}

fn resolve_registry_credential_provider(
    cargo_config: &CargoConfig,
    registry_name: Option<&str>,
) -> Option<SupportedRegistryCredentialProvider> {
    if let Some(registry_name) = registry_name {
        if let Some(provider) = get_registry_env_var(registry_name, "CREDENTIAL_PROVIDER") {
            return resolve_supported_provider_name(cargo_config, &provider, &mut Vec::new());
        }

        if let Some(provider) = cargo_config
            .get_registry(registry_name)
            .and_then(|registry| registry.credential_provider.as_ref())
        {
            return resolve_supported_provider_from_config(cargo_config, provider, &mut Vec::new());
        }
    }

    cargo_config
        .registry
        .as_ref()
        .and_then(|registry| registry.global_credential_providers.as_ref())
        .and_then(|providers| resolve_global_supported_provider(cargo_config, providers.iter()))
        .or(Some(SupportedRegistryCredentialProvider::CargoToken))
}

fn resolve_cargo_token(
    cargo_credentials: &Credentials,
    registry_name: Option<&str>,
) -> Option<SecretString> {
    if let Some(registry_name) = registry_name {
        if let Some(token) = get_registry_env_var(registry_name, "TOKEN") {
            return Some(SecretString::from_boxed_str(token.into_boxed_str()));
        }

        if let Some(token) = cargo_credentials.get_registry_token(registry_name) {
            return Some(token.clone());
        }
    }

    None
}

fn resolve_provider_command_arg(arg: &str, index_url: &str) -> String {
    // Cargo's `BasicProcessCredential` replaces `{index_url}` before spawning `cargo:token-from-stdout` commands.
    // https://github.com/rust-lang/cargo/blob/master/src/cargo/util/credential/adaptor.rs#L27-L34
    arg.replace("{index_url}", index_url)
}

fn resolve_cargo_token_from_stdout(
    provider_args: &[CompactString],
    registry_name: Option<&str>,
    registry: &Registry,
) -> io::Result<SecretString> {
    let Some(executable) = provider_args.first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The first argument to `cargo:token-from-stdout` must be a command that prints a token on stdout",
        ));
    };
    let index_url = registry.cargo_install_index_arg();

    let mut command = Command::new(executable.as_str());
    command
        .args(
            provider_args[1..]
                .iter()
                .map(|arg| resolve_provider_command_arg(arg, &index_url)),
        )
        .env(
            "CARGO",
            env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
        )
        .env("CARGO_REGISTRY_INDEX_URL", &index_url)
        .stdout(Stdio::piped());

    if let Some(name) = registry_name {
        command.env("CARGO_REGISTRY_NAME_OPT", name);
    }

    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take().unwrap();

    let mut buffer = SecretString::from_string(String::new());
    use std::io::Read as _;
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

pub(crate) fn resolve_registry_auth(
    cargo_config: &CargoConfig,
    cargo_home: &Path,
    registry_name: Option<&str>,
    registry: &Registry,
) -> Option<RegistryAuth> {
    let provider = resolve_registry_credential_provider(cargo_config, registry_name)?;
    let token = match provider {
        SupportedRegistryCredentialProvider::CargoToken => {
            let cargo_credentials = Credentials::load_from_home(cargo_home).ok()?;
            resolve_cargo_token(&cargo_credentials, registry_name)?
        }
        SupportedRegistryCredentialProvider::CargoTokenFromStdout(provider_args) => {
            resolve_cargo_token_from_stdout(&provider_args, registry_name, registry).ok()?
        }
    };

    RegistryAuth::new(registry_name.map(CompactString::from), token)
}

#[cfg(test)]
mod tests {
    use std::{env, io::Cursor, sync::Mutex};

    use super::*;
    use once_cell::sync::Lazy;
    use tempfile::tempdir;

    static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    #[test]
    fn test_get_registry_env_var_normalizes_registry_name() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN", "token");

        assert_eq!(
            get_registry_env_var("private-registry", "TOKEN").as_deref(),
            Some("token")
        );

        env::remove_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN");
    }

    #[test]
    fn test_global_provider_defaults_to_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::default();

        assert_eq!(
            resolve_registry_credential_provider(&config, Some("private-registry")),
            Some(SupportedRegistryCredentialProvider::CargoToken)
        );
    }

    #[test]
    fn test_registry_provider_env_takes_precedence_over_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var(
            "CARGO_REGISTRIES_PRIVATE_REGISTRY_CREDENTIAL_PROVIDER",
            "cargo:libsecret",
        );

        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert!(resolve_registry_credential_provider(&config, Some("private-registry")).is_none());

        env::remove_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_CREDENTIAL_PROVIDER");
    }

    #[test]
    fn test_registry_provider_alias_enables_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "custom"

[credential-alias]
custom = "cargo:token"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert_eq!(
            resolve_registry_credential_provider(&config, Some("private-registry")),
            Some(SupportedRegistryCredentialProvider::CargoToken)
        );
    }

    #[test]
    fn test_registry_provider_env_alias_enables_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var(
            "CARGO_REGISTRIES_PRIVATE_REGISTRY_CREDENTIAL_PROVIDER",
            "custom",
        );

        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:libsecret"

[credential-alias]
custom = "cargo:token"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert_eq!(
            resolve_registry_credential_provider(&config, Some("private-registry")),
            Some(SupportedRegistryCredentialProvider::CargoToken)
        );

        env::remove_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_CREDENTIAL_PROVIDER");
    }

    #[test]
    fn test_registry_provider_alias_non_token_disables_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "custom"

[credential-alias]
custom = ["cargo-credential-example", "--account", "test"]
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert!(resolve_registry_credential_provider(&config, Some("private-registry")).is_none());
    }

    #[test]
    fn test_registry_provider_array_alias_enables_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = ["custom"]

[credential-alias]
custom = "cargo:token"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert_eq!(
            resolve_registry_credential_provider(&config, Some("private-registry")),
            Some(SupportedRegistryCredentialProvider::CargoToken)
        );
    }

    #[test]
    fn test_registry_provider_builtin_cargo_names_do_not_use_aliases() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token-from-stdout"

[credential-alias]
"cargo:token-from-stdout" = "cargo:token"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert!(resolve_registry_credential_provider(&config, Some("private-registry")).is_none());
    }

    #[test]
    fn test_registry_provider_string_enables_cargo_token_from_stdout() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token-from-stdout rustc --print sysroot"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert_eq!(
            resolve_registry_credential_provider(&config, Some("private-registry")),
            Some(SupportedRegistryCredentialProvider::CargoTokenFromStdout(
                vec!["rustc".into(), "--print".into(), "sysroot".into()]
            ))
        );
    }

    #[test]
    fn test_registry_provider_alias_enables_cargo_token_from_stdout() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "custom"

[credential-alias]
custom = ["cargo:token-from-stdout", "rustc", "--print", "sysroot"]
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert_eq!(
            resolve_registry_credential_provider(&config, Some("private-registry")),
            Some(SupportedRegistryCredentialProvider::CargoTokenFromStdout(
                vec!["rustc".into(), "--print".into(), "sysroot".into()]
            ))
        );
    }

    #[test]
    fn test_global_provider_prefers_last_supported_provider() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registry]
global-credential-providers = [
    "cargo:token",
    "cargo:token-from-stdout rustc --print sysroot",
]
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();

        assert_eq!(
            resolve_registry_credential_provider(&config, Some("private-registry")),
            Some(SupportedRegistryCredentialProvider::CargoTokenFromStdout(
                vec!["rustc".into(), "--print".into(), "sysroot".into()]
            ))
        );
    }

    #[test]
    fn test_resolve_registry_auth_uses_token_from_stdout_provider() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token-from-stdout rustc --print sysroot"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();
        let tempdir = tempdir().unwrap();
        let registry: Registry = "sparse+https://registry.example.com/index/"
            .parse()
            .unwrap();

        let auth =
            resolve_registry_auth(&config, tempdir.path(), Some("private-registry"), &registry)
                .unwrap();

        assert!(!auth.token().is_empty());
        assert_eq!(auth.registry_name(), Some("private-registry"));
    }

    #[test]
    fn test_resolve_registry_auth_uses_credentials_when_cargo_token_is_enabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();
        let tempdir = tempdir().unwrap();
        std::fs::write(
            tempdir.path().join("credentials.toml"),
            r#"
[registries.private-registry]
token = "secret-token"
            "#,
        )
        .unwrap();
        let registry: Registry = "sparse+https://registry.example.com/index/"
            .parse()
            .unwrap();

        let auth =
            resolve_registry_auth(&config, tempdir.path(), Some("private-registry"), &registry)
                .unwrap();

        assert_eq!(auth.token(), "secret-token");
        assert_eq!(auth.registry_name(), Some("private-registry"));
    }

    #[test]
    fn test_resolve_registry_auth_uses_env_token_when_cargo_token_is_enabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN", "secret-token");

        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();
        let tempdir = tempdir().unwrap();
        let registry: Registry = "sparse+https://registry.example.com/index/"
            .parse()
            .unwrap();

        let auth =
            resolve_registry_auth(&config, tempdir.path(), Some("private-registry"), &registry)
                .unwrap();

        assert_eq!(auth.token(), "secret-token");
        assert_eq!(auth.registry_name(), Some("private-registry"));

        env::remove_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN");
    }

    #[test]
    fn test_resolve_registry_auth_ignores_config_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::remove_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN");

        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:token"
token = "secret-token"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();
        let tempdir = tempdir().unwrap();
        let registry: Registry = "sparse+https://registry.example.com/index/"
            .parse()
            .unwrap();

        assert!(resolve_registry_auth(
            &config,
            tempdir.path(),
            Some("private-registry"),
            &registry,
        )
        .is_none());

        env::remove_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN");
    }

    #[test]
    fn test_resolve_registry_auth_ignores_token_when_provider_is_not_cargo_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        env::set_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN", "secret-token");

        let config = CargoConfig::load_from_reader(
            Cursor::new(
                r#"
[registries.private-registry]
index = "sparse+https://registry.example.com/index/"
credential-provider = "cargo:libsecret"
                "#,
            ),
            std::path::Path::new("."),
        )
        .unwrap();
        let tempdir = tempdir().unwrap();
        let registry: Registry = "sparse+https://registry.example.com/index/"
            .parse()
            .unwrap();

        assert!(resolve_registry_auth(
            &config,
            tempdir.path(),
            Some("private-registry"),
            &registry,
        )
        .is_none());

        env::remove_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_TOKEN");
    }
}
