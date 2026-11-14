use std::{fmt, sync::Arc, time::Duration};

use binstalk_types::SecretString;
use compact_str::CompactString;

use crate::Registry;

#[derive(Clone, Debug)]
pub struct RegistryAuth {
    registry_name: Option<CompactString>,
    token: Arc<SecretString>,
}

impl RegistryAuth {
    pub fn new(registry_name: Option<CompactString>, token: SecretString) -> Option<Self> {
        if token.is_empty() {
            None
        } else {
            Some(Self {
                registry_name,
                token: Arc::new(token),
            })
        }
    }

    pub fn token(&self) -> &str {
        &self.token[..]
    }

    pub fn registry_name(&self) -> Option<&str> {
        self.registry_name.as_deref()
    }
}

#[derive(Clone)]
pub struct ResolvedRegistry {
    registry: Registry,
    auth: Option<RegistryAuth>,
    min_publish_age: Option<Duration>,
}

impl ResolvedRegistry {
    pub fn new(registry: Registry, auth: Option<RegistryAuth>) -> Self {
        Self {
            registry,
            auth,
            min_publish_age: None,
        }
    }

    /// Skip versions published less than `min_publish_age` ago, like Cargo's
    /// `min-publish-age`. [`Duration::ZERO`] disables it.
    ///
    /// Versions without a publish time in the registry index are not affected.
    pub fn with_min_publish_age(self, min_publish_age: Duration) -> Self {
        Self {
            min_publish_age: Some(min_publish_age),
            ..self
        }
    }

    pub async fn fetch_crate_matched(
        &self,
        client: binstalk_downloader::remote::Client,
        crate_name: &str,
        version_req: &semver::VersionReq,
    ) -> Result<
        (
            cargo_toml_workspace::cargo_toml::Manifest<binstalk_types::cargo_toml_binstall::Meta>,
            Option<CompactString>,
        ),
        crate::RegistryError,
    > {
        self.registry
            .fetch_crate_matched_with_auth(
                client,
                self.auth.as_ref(),
                crate_name,
                version_req,
                self.min_publish_age,
            )
            .await
    }

    pub fn crate_source(&self) -> Result<binstalk_types::crate_info::CrateSource, url::ParseError> {
        self.registry.crate_source()
    }

    pub fn cargo_install_index_arg(&self) -> String {
        self.registry.cargo_install_index_arg()
    }
}

impl fmt::Debug for ResolvedRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedRegistry")
            .field("registry", &self.registry)
            .field("auth", &self.auth)
            .field("min_publish_age", &self.min_publish_age)
            .finish()
    }
}

impl Default for ResolvedRegistry {
    fn default() -> Self {
        Self::new(Registry::default(), None)
    }
}

impl fmt::Display for ResolvedRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.registry, f)
    }
}
