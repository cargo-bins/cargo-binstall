use std::{error::Error as StdError, fmt, sync::Arc, time::Duration};

use binstalk_types::SecretString;
use compact_str::CompactString;
use tokio::{sync::OnceCell, task::spawn_blocking};
use url::Url;

use crate::{Registry, RegistryError};

/// Error returned by a [`CredentialProvider`].
pub type CredentialProviderError = Box<dyn StdError + Send + Sync + 'static>;

/// Gets a token for a registry the first time the registry asks for one.
///
/// It runs on a blocking thread, so it may spawn processes or prompt the
/// user.
pub trait CredentialProvider: Send + Sync + 'static {
    /// * `headers` - headers of the `401 Unauthorized` response that made the
    ///   registry ask for a token, formatted as `name: value`. It is empty if
    ///   the registry asked for a token through `auth-required` in its
    ///   `config.json`.
    ///
    /// Return `Ok(None)` if no token is found for this registry.
    fn get_token(
        &self,
        headers: &[String],
    ) -> Result<Option<SecretString>, CredentialProviderError>;
}

enum Token {
    Found(SecretString),
    NotFound,
    Failed(Arc<dyn StdError + Send + Sync + 'static>),
}

#[derive(Clone)]
pub struct RegistryAuth {
    registry_name: Option<CompactString>,
    token: Arc<OnceCell<Token>>,
    provider: Option<Arc<dyn CredentialProvider>>,
}

impl RegistryAuth {
    pub fn new(registry_name: Option<CompactString>, token: SecretString) -> Option<Self> {
        if token.is_empty() {
            None
        } else {
            Some(Self {
                registry_name,
                token: Arc::new(OnceCell::new_with(Some(Token::Found(token)))),
                provider: None,
            })
        }
    }

    /// Get the token from `provider` once the registry asks for it.
    ///
    /// The provider runs at most once, and its result (token, not found or
    /// error) is reused for the rest of the session.
    pub fn with_provider(
        registry_name: Option<CompactString>,
        provider: Arc<dyn CredentialProvider>,
    ) -> Self {
        Self {
            registry_name,
            token: Arc::new(OnceCell::new()),
            provider: Some(provider),
        }
    }

    /// Return the token, getting it from the provider on first use.
    ///
    /// * `url` - the url that needs the token, used in errors.
    /// * `headers` - see [`CredentialProvider::get_token`].
    pub(crate) async fn token(
        &self,
        url: &Url,
        headers: Vec<String>,
    ) -> Result<&str, RegistryError> {
        let token = self
            .token
            .get_or_init(|| async {
                let Some(provider) = self.provider.clone() else {
                    return Token::NotFound;
                };

                match spawn_blocking(move || provider.get_token(&headers)).await {
                    Ok(Ok(Some(token))) if !token.is_empty() => Token::Found(token),
                    Ok(Ok(_)) => Token::NotFound,
                    Ok(Err(err)) => Token::Failed(err.into()),
                    Err(err) => Token::Failed(Arc::new(err)),
                }
            })
            .await;

        match token {
            Token::Found(token) => Ok(&token[..]),
            Token::NotFound => Err(RegistryError::AuthenticationRequired(Box::new(url.clone()))),
            Token::Failed(err) => Err(RegistryError::CredentialProvider {
                registry: match &self.registry_name {
                    Some(name) => name.clone(),
                    None => url.as_str().into(),
                },
                source: err.clone(),
            }),
        }
    }

    pub fn registry_name(&self) -> Option<&str> {
        self.registry_name.as_deref()
    }
}

impl fmt::Debug for RegistryAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistryAuth")
            .field("registry_name", &self.registry_name)
            .field("token_resolved", &self.token.initialized())
            .field("has_provider", &self.provider.is_some())
            .finish()
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

    /// Return the min-publish-age set by [`ResolvedRegistry::with_min_publish_age`].
    pub fn min_publish_age(&self) -> Option<Duration> {
        self.min_publish_age
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct CountingProvider {
        calls: AtomicUsize,
        result: fn() -> Result<Option<SecretString>, CredentialProviderError>,
    }

    impl CredentialProvider for CountingProvider {
        fn get_token(
            &self,
            headers: &[String],
        ) -> Result<Option<SecretString>, CredentialProviderError> {
            assert_eq!(headers, ["www-authenticate: Cargo"]);
            self.calls.fetch_add(1, Ordering::Relaxed);
            (self.result)()
        }
    }

    fn auth(
        result: fn() -> Result<Option<SecretString>, CredentialProviderError>,
    ) -> (RegistryAuth, Arc<CountingProvider>) {
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
            result,
        });
        let auth = RegistryAuth::with_provider(Some("private".into()), provider.clone());
        (auth, provider)
    }

    fn url() -> Url {
        Url::parse("https://registry.example.com/index/config.json").unwrap()
    }

    fn headers() -> Vec<String> {
        vec!["www-authenticate: Cargo".into()]
    }

    #[tokio::test]
    async fn test_provider_runs_once_on_first_use() {
        let (auth, provider) = auth(|| Ok(Some(SecretString::from_string("token".into()))));
        assert_eq!(provider.calls.load(Ordering::Relaxed), 0);

        assert_eq!(auth.token(&url(), headers()).await.unwrap(), "token");
        assert_eq!(
            auth.clone().token(&url(), headers()).await.unwrap(),
            "token"
        );
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_provider_not_found_is_authentication_required() {
        let (auth, provider) = auth(|| Ok(None));

        for _ in 0..2 {
            assert!(matches!(
                auth.token(&url(), headers()).await,
                Err(RegistryError::AuthenticationRequired(_))
            ));
        }
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);

        let (empty, _) = self::auth(|| Ok(Some(SecretString::default())));
        assert!(matches!(
            empty.token(&url(), headers()).await,
            Err(RegistryError::AuthenticationRequired(_))
        ));
    }

    #[tokio::test]
    async fn test_provider_error_is_kept() {
        let (auth, provider) = auth(|| Err("vault is locked".into()));

        for _ in 0..2 {
            match auth.token(&url(), headers()).await {
                Err(RegistryError::CredentialProvider { registry, source }) => {
                    assert_eq!(registry, "private");
                    assert_eq!(source.to_string(), "vault is locked");
                }
                res => panic!("unexpected {res:?}"),
            }
        }
        assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_static_token() {
        assert!(RegistryAuth::new(None, SecretString::default()).is_none());

        let auth = RegistryAuth::new(None, SecretString::from_string("token".into())).unwrap();
        assert_eq!(auth.token(&url(), Vec::new()).await.unwrap(), "token");
    }
}
