use std::{fmt, sync::Arc};

use compact_str::CompactString;
use zeroize::Zeroizing;

use crate::Registry;

#[derive(Clone)]
pub struct RegistryAuth {
    registry_name: Option<CompactString>,
    token: Arc<Zeroizing<Box<str>>>,
}

impl RegistryAuth {
    pub fn new(
        registry_name: Option<CompactString>,
        token: impl Into<Zeroizing<Box<str>>>,
    ) -> Option<Self> {
        let token = token.into();

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
        self.token.as_ref().as_ref()
    }

    pub fn registry_name(&self) -> Option<&str> {
        self.registry_name.as_deref()
    }
}

impl fmt::Debug for RegistryAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistryAuth")
            .field("registry_name", &self.registry_name)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Clone)]
pub struct ResolvedRegistry {
    registry: Registry,
    auth: Option<RegistryAuth>,
}

impl ResolvedRegistry {
    pub fn new(registry: Registry, auth: Option<RegistryAuth>) -> Self {
        Self { registry, auth }
    }

    pub async fn fetch_crate_matched(
        &self,
        client: binstalk_downloader::remote::Client,
        crate_name: &str,
        version_req: &semver::VersionReq,
    ) -> Result<
        cargo_toml_workspace::cargo_toml::Manifest<binstalk_types::cargo_toml_binstall::Meta>,
        crate::RegistryError,
    > {
        self.registry
            .fetch_crate_matched_with_auth(client, self.auth.as_ref(), crate_name, version_req)
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
