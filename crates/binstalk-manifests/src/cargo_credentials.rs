//! Cargo's credentials file.
//!
//! Cargo stores plaintext registry tokens in `$CARGO_HOME/credentials.toml`
//! or the legacy `$CARGO_HOME/credentials` path.

use std::{fs::File, io, path::Path};

use binstalk_types::SecretString;
use compact_str::CompactString;
use exn::{ErrorExt, ResultExt};
use fs_lock::FileLock;
use miette::Diagnostic;
use serde::{Deserialize, Deserializer};
use thiserror::Error;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RegistryCredential {
    #[serde(default, deserialize_with = "deserialize_secret_string")]
    pub token: Option<SecretString>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Credentials {
    pub registries: Option<std::collections::BTreeMap<CompactString, RegistryCredential>>,
    pub registry: Option<RegistryCredential>,
}

impl Credentials {
    pub fn load_from_reader<R: io::Read>(reader: R) -> exn::Result<Self, CredentialsLoadError> {
        let mut reader = reader;
        let mut contents = Vec::new();
        reader
            .read_to_end(&mut contents)
            .or_raise(|| CredentialsLoadError::Io)?;
        let credentials: Credentials =
            toml_edit::de::from_slice(&contents).or_raise(|| CredentialsLoadError::TomlParse)?;
        Ok(credentials)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> exn::Result<Self, CredentialsLoadError> {
        match File::open(path.as_ref()) {
            Ok(file) => {
                let file = FileLock::new_shared(file)
                    .or_raise(|| CredentialsLoadError::Io)?
                    .set_file_path(path.as_ref());
                Self::load_from_reader(file)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Default::default()),
            Err(err) => Err(err.raise().raise(CredentialsLoadError::Io)),
        }
    }

    pub fn load_from_home(cargo_home: impl AsRef<Path>) -> exn::Result<Self, CredentialsLoadError> {
        let cargo_home = cargo_home.as_ref();
        let toml_path = cargo_home.join("credentials.toml");

        match File::open(&toml_path) {
            Ok(file) => {
                let file = FileLock::new_shared(file)
                    .or_raise(|| CredentialsLoadError::Io)?
                    .set_file_path(toml_path.as_path());
                Self::load_from_reader(file)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                Self::load_from_path(cargo_home.join("credentials"))
            }
            Err(err) => Err(err.raise().raise(CredentialsLoadError::Io)),
        }
    }

    pub fn get_registry_token(&self, name: &str) -> Option<&SecretString> {
        self.registries.as_ref()?.get(name)?.token.as_ref()
    }
}

fn deserialize_secret_string<'de, D>(deserializer: D) -> Result<Option<SecretString>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<Box<str>>::deserialize(deserializer)
        .map(|value| value.map(SecretString::from_boxed_str))
}

/// The underlying cause is attached as a child frame of the [`exn::Exn`].
#[derive(Debug, Diagnostic, Error)]
#[non_exhaustive]
pub enum CredentialsLoadError {
    #[error("I/O Error")]
    Io,

    #[error("Failed to deserialize toml")]
    TomlParse,
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Cursor};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_loading() {
        const CREDENTIALS: &str = r#"
[registry]
token = "crates-io-token"

[registries.private-registry]
token = "private-token"
        "#;

        let credentials = Credentials::load_from_reader(Cursor::new(CREDENTIALS)).unwrap();

        assert_eq!(
            credentials
                .get_registry_token("private-registry")
                .map(|token| &token[..]),
            Some("private-token")
        );
    }

    #[test]
    fn test_load_from_home_prefers_toml_path() {
        let tempdir = tempdir().unwrap();
        let home = tempdir.path();

        fs::write(
            home.join("credentials"),
            "[registries.example]\ntoken = \"legacy\"\n",
        )
        .unwrap();
        fs::write(
            home.join("credentials.toml"),
            "[registries.example]\ntoken = \"toml\"\n",
        )
        .unwrap();

        let credentials = Credentials::load_from_home(home).unwrap();

        assert_eq!(
            credentials
                .get_registry_token("example")
                .map(|token| &token[..]),
            Some("toml")
        );
    }

    #[test]
    fn test_registry_credential_debug_redacts_token() {
        let credential = RegistryCredential {
            token: Some(SecretString::from_boxed_str("secret-token".into())),
        };

        let debug = format!("{credential:?}");

        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("<redacted>"));
    }
}
