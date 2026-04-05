use std::{env, path::Path};

use binstalk::registry::{Registry, RegistryAuth};
use binstalk_manifests::{
    cargo_config::{Config as CargoConfig, CredentialProvider},
    cargo_credentials::Credentials,
};
use compact_str::CompactString;
use zeroize::Zeroizing;

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
    let suffix = suffix.to_ascii_uppercase();

    env::vars().find_map(|(key, value)| {
        let registry_name = key
            .strip_prefix("CARGO_REGISTRIES_")?
            .strip_suffix(&format!("_{suffix}"))?;

        (normalize_registry_name(registry_name) == normalized_name).then_some(value)
    })
}

fn provider_supports_cargo_token(provider: &CredentialProvider) -> bool {
    match provider {
        CredentialProvider::String(provider) => provider == "cargo:token",
        CredentialProvider::Array(provider) => provider.len() == 1 && provider[0] == "cargo:token",
    }
}

fn global_provider_supports_cargo_token(providers: &[CompactString]) -> bool {
    providers.iter().any(|provider| provider == "cargo:token")
}

fn cargo_token_provider_enabled(cargo_config: &CargoConfig, registry_name: Option<&str>) -> bool {
    if let Some(registry_name) = registry_name {
        if let Some(provider) = get_registry_env_var(registry_name, "CREDENTIAL_PROVIDER") {
            return provider == "cargo:token";
        }

        if let Some(provider) = cargo_config
            .get_registry(registry_name)
            .and_then(|registry| registry.credential_provider.as_ref())
        {
            return provider_supports_cargo_token(provider);
        }
    }

    cargo_config
        .registry
        .as_ref()
        .and_then(|registry| registry.global_credential_providers.as_deref())
        .map(global_provider_supports_cargo_token)
        .unwrap_or(true)
}

fn resolve_cargo_token(
    cargo_config: &CargoConfig,
    cargo_credentials: &Credentials,
    registry_name: Option<&str>,
) -> Option<Zeroizing<Box<str>>> {
    if let Some(registry_name) = registry_name {
        if let Some(token) = get_registry_env_var(registry_name, "TOKEN") {
            return Some(Zeroizing::new(token.into_boxed_str()));
        }

        if let Some(token) = cargo_credentials.get_registry_token(registry_name) {
            return Some(Zeroizing::new(token.into()));
        }

        if let Some(token) = cargo_config
            .get_registry(registry_name)
            .and_then(|registry| registry.token.as_ref().map(|token| &token[..]))
        {
            return Some(Zeroizing::new(token.into()));
        }
    }

    None
}

pub(crate) fn resolve_registry_auth(
    cargo_config: &CargoConfig,
    cargo_home: &Path,
    registry_name: Option<&str>,
    _registry: &Registry,
) -> Option<RegistryAuth> {
    if !cargo_token_provider_enabled(cargo_config, registry_name) {
        return None;
    }

    let cargo_credentials = Credentials::load_from_home(cargo_home).ok()?;
    let token = resolve_cargo_token(cargo_config, &cargo_credentials, registry_name)?;

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
        let config = CargoConfig::default();

        assert!(cargo_token_provider_enabled(
            &config,
            Some("private-registry")
        ));
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

        assert!(!cargo_token_provider_enabled(
            &config,
            Some("private-registry")
        ));

        env::remove_var("CARGO_REGISTRIES_PRIVATE_REGISTRY_CREDENTIAL_PROVIDER");
    }

    #[test]
    fn test_resolve_registry_auth_uses_credentials_when_cargo_token_is_enabled() {
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
