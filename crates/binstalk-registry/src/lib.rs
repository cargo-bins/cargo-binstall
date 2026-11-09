#![cfg_attr(docsrs, feature(doc_cfg))]

use std::{fmt, str::FromStr, sync::Arc};

use binstalk_downloader::remote::Client;
use binstalk_types::{
    cargo_toml_binstall::Meta,
    crate_info::{CrateSource, SourceType},
    maybe_owned::MaybeOwned,
};
use cargo_toml_workspace::cargo_toml::Manifest;
use compact_str::CompactString;
use miette::Diagnostic;
use semver::VersionReq;
use thiserror::Error as ThisError;
use url::{ParseError as UrlParseError, Url};

mod auth;
pub use auth::{RegistryAuth, ResolvedRegistry};

#[cfg(feature = "git")]
pub use simple_git::{GitError, GitUrl, GitUrlParseError};

mod vfs;

mod visitor;

mod common;
use common::*;

#[cfg(feature = "git")]
mod git_registry;
#[cfg(feature = "git")]
pub use git_registry::GitRegistry;

#[cfg(any(feature = "crates_io_api", test))]
mod crates_io_registry;
#[cfg(any(feature = "crates_io_api", test))]
pub use crates_io_registry::fetch_crate_cratesio_api;

mod sparse_registry;
pub use sparse_registry::SparseRegistry;

/// Error returned by [`Registry`].
///
/// The underlying cause is attached as a child frame of the [`exn::Exn`].
#[derive(Debug, ThisError, Diagnostic)]
#[diagnostic(severity(error), code(binstall::cargo_registry))]
#[non_exhaustive]
pub enum RegistryError {
    #[error("Failed to fetch from remote")]
    Remote,

    #[error("{0} is not found")]
    #[diagnostic(
        help("Check that the crate name you provided is correct.\nYou can also search for a matching crate at: https://lib.rs/search?q={0}")
    )]
    NotFound(CompactString),

    #[error("Failed to parse json")]
    Json,

    #[error("Failed to parse dl config")]
    ParseDlConfig,

    #[error("Failed to render dl config")]
    RenderDlConfig,

    #[error("Failed to parse checksum encoded in hex")]
    InvalidHex,

    #[error("Expected checksum `{expected}`, actual checksum `{actual}`")]
    UnmatchedChecksum {
        expected: Box<str>,
        actual: Box<str>,
    },

    #[error("no version matching requirement '{req}'")]
    VersionMismatch { req: semver::VersionReq },

    #[error("Failed to parse cargo manifest")]
    #[diagnostic(help("If you used --manifest-path, check the Cargo.toml syntax."))]
    CargoManifest,

    #[error("Failed to parse url")]
    UrlParse,

    #[error("Failed to download crate")]
    Download,

    #[error("I/O Error")]
    Io,

    #[error("Failed to join task")]
    TaskJoinError,

    #[cfg(feature = "git")]
    #[error("Failed to shallow clone git repository")]
    GitError,

    #[error("registry requires authentication: {0}")]
    #[diagnostic(
        help("Configure Cargo credentials for this registry, for example with `cargo login` or `CARGO_REGISTRIES_<NAME>_TOKEN` when `cargo:token` is active.")
    )]
    AuthenticationRequired(Box<Url>),
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Registry {
    Sparse(Arc<SparseRegistry>),

    #[cfg(feature = "git")]
    Git(GitRegistry),
}

impl Default for Registry {
    fn default() -> Self {
        Self::crates_io_sparse_registry()
    }
}

#[derive(Debug, ThisError)]
#[error("Invalid registry `{src}`, {inner}")]
pub struct InvalidRegistryError {
    src: CompactString,
    #[source]
    inner: InvalidRegistryErrorInner,
}

#[derive(Debug, ThisError)]
enum InvalidRegistryErrorInner {
    #[cfg(feature = "git")]
    #[error("failed to parse git url {0}")]
    GitUrlParseErr(#[from] Box<GitUrlParseError>),

    #[error("failed to parse sparse registry url: {0}")]
    UrlParseErr(#[from] UrlParseError),

    #[error("expected protocol http(s), actual url `{0}`")]
    InvalidScheme(Box<Url>),

    #[cfg(not(feature = "git"))]
    #[error("git registry not supported")]
    GitRegistryNotSupported,
}

impl Registry {
    /// Return a crates.io sparse registry
    pub fn crates_io_sparse_registry() -> Self {
        Self::Sparse(Arc::new(SparseRegistry::new(
            Url::parse("https://index.crates.io/").unwrap(),
        )))
    }

    fn from_str_inner(s: &str) -> Result<Self, InvalidRegistryErrorInner> {
        if let Some(s) = s.strip_prefix("sparse+") {
            let url = Url::parse(s.trim_end_matches('/'))?;

            let scheme = url.scheme();
            if scheme != "http" && scheme != "https" {
                Err(InvalidRegistryErrorInner::InvalidScheme(Box::new(url)))
            } else {
                Ok(Self::Sparse(Arc::new(SparseRegistry::new(url))))
            }
        } else {
            #[cfg(not(feature = "git"))]
            {
                Err(InvalidRegistryErrorInner::GitRegistryNotSupported)
            }
            #[cfg(feature = "git")]
            {
                let url = GitUrl::from_str(s).map_err(Box::new)?;
                Ok(Self::Git(GitRegistry::new(url)))
            }
        }
    }

    /// Fetch the latest crate with `crate_name` and with version matching
    /// `version_req`.
    pub async fn fetch_crate_matched(
        &self,
        client: Client,
        crate_name: &str,
        version_req: &VersionReq,
    ) -> exn::Result<Manifest<Meta>, RegistryError> {
        self.fetch_crate_matched_with_auth(client, None, crate_name, version_req)
            .await
    }

    pub(crate) async fn fetch_crate_matched_with_auth(
        &self,
        client: Client,
        auth: Option<&RegistryAuth>,
        crate_name: &str,
        version_req: &VersionReq,
    ) -> exn::Result<Manifest<Meta>, RegistryError> {
        match self {
            Self::Sparse(sparse_registry) => {
                sparse_registry
                    .fetch_crate_matched(client, auth, crate_name, version_req)
                    .await
            }
            #[cfg(feature = "git")]
            Self::Git(git_registry) => {
                git_registry
                    .fetch_crate_matched(client, auth, crate_name, version_req)
                    .await
            }
        }
    }

    /// Get url of the registry
    pub fn url(&self) -> Result<MaybeOwned<'_, Url>, UrlParseError> {
        match self {
            #[cfg(feature = "git")]
            Registry::Git(registry) => {
                Url::parse(&registry.url().to_string()).map(MaybeOwned::Owned)
            }
            Registry::Sparse(registry) => Ok(MaybeOwned::Borrowed(registry.url())),
        }
    }

    /// Get crate source of this registry
    pub fn crate_source(&self) -> Result<CrateSource, UrlParseError> {
        let registry = self.url()?;
        let source_type = match self {
            #[cfg(feature = "git")]
            Registry::Git(_) => SourceType::Git,
            Registry::Sparse(_) => SourceType::Sparse,
        };

        Ok(match (registry.as_str(), source_type) {
            ("https://index.crates.io/", SourceType::Sparse)
            | ("https://github.com/rust-lang/crates.io-index", SourceType::Git) => {
                CrateSource::cratesio_registry()
            }
            _ => CrateSource {
                source_type,
                url: MaybeOwned::Owned(registry.into_owned()),
            },
        })
    }

    pub fn cargo_install_index_arg(&self) -> String {
        match self {
            #[cfg(feature = "git")]
            Registry::Git(registry) => registry.url().to_string(),
            Registry::Sparse(registry) => format!("sparse+{}", registry.url()),
        }
    }
}

impl fmt::Display for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(feature = "git")]
            Registry::Git(registry) => fmt::Display::fmt(&registry.url(), f),
            Registry::Sparse(registry) => fmt::Display::fmt(&registry.url(), f),
        }
    }
}

impl FromStr for Registry {
    type Err = InvalidRegistryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str_inner(s).map_err(|inner| InvalidRegistryError {
            src: s.into(),
            inner,
        })
    }
}

#[cfg(test)]
mod test {
    use std::num::NonZeroU16;

    use toml_edit::ser::to_string;

    use super::*;

    /// Mark this as an async fn so that you won't accidentally use it in
    /// sync context.
    fn create_client() -> Client {
        Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
            NonZeroU16::new(10).unwrap(),
            1.try_into().unwrap(),
            [],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_crates_io_sparse_registry() {
        let client = create_client();

        let crate_name = "cargo-binstall";
        let version_req = &VersionReq::parse("=1.0.0").unwrap();

        let serialized_manifest_from_sparse_task = tokio::spawn({
            let client = client.clone();
            let version_req = version_req.clone();

            async move {
                let sparse_registry: Registry = Registry::crates_io_sparse_registry();
                assert!(
                    matches!(sparse_registry, Registry::Sparse(_)),
                    "{:?}",
                    sparse_registry
                );

                let manifest_from_sparse = sparse_registry
                    .fetch_crate_matched(client, crate_name, &version_req)
                    .await
                    .unwrap();

                to_string(&manifest_from_sparse).unwrap()
            }
        });

        let manifest_from_cratesio_api = fetch_crate_cratesio_api(client, crate_name, version_req)
            .await
            .unwrap();

        let serialized_manifest_from_cratesio_api = to_string(&manifest_from_cratesio_api).unwrap();

        assert_eq!(
            serialized_manifest_from_sparse_task.await.unwrap(),
            serialized_manifest_from_cratesio_api
        );
    }

    #[cfg(feature = "git")]
    #[tokio::test]
    async fn test_crates_io_git_registry() {
        let client = create_client();

        let crate_name = "cargo-binstall";
        let version_req = &VersionReq::parse("=1.0.0").unwrap();

        let serialized_manifest_from_git_task = tokio::spawn({
            let version_req = version_req.clone();
            let client = client.clone();

            async move {
                let git_registry: Registry = "https://github.com/rust-lang/crates.io-index"
                    .parse()
                    .unwrap();
                assert!(
                    matches!(git_registry, Registry::Git(_)),
                    "{:?}",
                    git_registry
                );

                let manifest_from_git = git_registry
                    .fetch_crate_matched(client, crate_name, &version_req)
                    .await
                    .unwrap();
                to_string(&manifest_from_git).unwrap()
            }
        });

        let manifest_from_cratesio_api = Registry::default()
            .fetch_crate_matched(client, crate_name, version_req)
            .await
            .unwrap();

        let serialized_manifest_from_cratesio_api = to_string(&manifest_from_cratesio_api).unwrap();

        assert_eq!(
            serialized_manifest_from_git_task.await.unwrap(),
            serialized_manifest_from_cratesio_api
        );
    }
}
