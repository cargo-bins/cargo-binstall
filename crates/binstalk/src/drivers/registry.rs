use std::{str::FromStr, sync::Arc};

use base16::DecodeError as Base16DecodeError;
use compact_str::CompactString;
use leon::{ParseError, RenderError};
use miette::Diagnostic;
use semver::VersionReq;
use serde_json::Error as JsonError;
use thiserror::Error as ThisError;

use crate::{
    errors::BinstallError,
    helpers::{
        cargo_toml::Manifest,
        remote::{Client, Error as RemoteError, Url, UrlParseError},
    },
    manifests::cargo_toml_binstall::Meta,
};

#[cfg(feature = "git")]
pub use crate::helpers::git::{GitUrl, GitUrlParseError};

mod vfs;

mod visitor;

mod common;
use common::*;

#[cfg(feature = "git")]
mod git_registry;
#[cfg(feature = "git")]
pub use git_registry::GitRegistry;

mod crates_io_registry;
pub use crates_io_registry::{fetch_crate_cratesio, CratesIoRateLimit};

mod sparse_registry;
pub use sparse_registry::SparseRegistry;

#[derive(Debug, ThisError, Diagnostic)]
#[diagnostic(severity(error), code(binstall::cargo_registry))]
#[non_exhaustive]
pub enum RegistryError {
    #[error(transparent)]
    Remote(#[from] RemoteError),

    #[error("{0} is not found")]
    #[diagnostic(
        help("Check that the crate name you provided is correct.\nYou can also search for a matching crate at: https://lib.rs/search?q={0}")
    )]
    NotFound(CompactString),

    #[error(transparent)]
    Json(#[from] JsonError),

    #[error("Failed to parse dl config: {0}")]
    ParseDlConfig(#[from] ParseError),

    #[error("Failed to render dl config: {0}")]
    RenderDlConfig(#[from] RenderError),

    #[error("Failed to parse checksum encoded in hex: {0}")]
    InvalidHex(#[from] Base16DecodeError),

    #[error("Expected checksum `{expected}`, actual checksum `{actual}`")]
    UnmatchedChecksum { expected: String, actual: String },
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Registry {
    CratesIo(Arc<CratesIoRateLimit>),

    Sparse(Arc<SparseRegistry>),

    #[cfg(feature = "git")]
    Git(GitRegistry),
}

impl Default for Registry {
    fn default() -> Self {
        Self::CratesIo(Default::default())
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

    #[error("expected protocol http(s), actual protocl {0}")]
    InvalidScheme(CompactString),

    #[cfg(not(feature = "git"))]
    #[error("git registry not supported")]
    GitRegistryNotSupported,
}

impl Registry {
    fn from_str_inner(s: &str) -> Result<Self, InvalidRegistryErrorInner> {
        if let Some(s) = s.strip_prefix("sparse+") {
            let url = Url::parse(s)?;

            let scheme = url.scheme();
            if scheme != "http" && scheme != "https" {
                Err(InvalidRegistryErrorInner::InvalidScheme(scheme.into()))
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
    ) -> Result<Manifest<Meta>, BinstallError> {
        match self {
            Self::CratesIo(rate_limit) => {
                fetch_crate_cratesio(client, crate_name, version_req, rate_limit).await
            }
            Self::Sparse(sparse_registry) => {
                sparse_registry
                    .fetch_crate_matched(client, crate_name, version_req)
                    .await
            }
            #[cfg(feature = "git")]
            Self::Git(git_registry) => {
                git_registry
                    .fetch_crate_matched(client, crate_name, version_req)
                    .await
            }
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
    async fn create_client() -> Client {
        Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            NonZeroU16::new(10).unwrap(),
            1.try_into().unwrap(),
            [],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_crates_io_sparse_registry() {
        let client = create_client().await;

        let sparse_registry: Registry = "sparse+https://index.crates.io/".parse().unwrap();
        assert!(
            matches!(sparse_registry, Registry::Sparse(_)),
            "{:?}",
            sparse_registry
        );

        let crate_name = "cargo-binstall";
        let version_req = &VersionReq::parse("=1.0.0").unwrap();
        let manifest_from_sparse = sparse_registry
            .fetch_crate_matched(client.clone(), crate_name, version_req)
            .await
            .unwrap();

        let manifest_from_cratesio_api = Registry::default()
            .fetch_crate_matched(client, crate_name, version_req)
            .await
            .unwrap();

        let serialized_manifest_from_sparse = to_string(&manifest_from_sparse).unwrap();
        let serialized_manifest_from_cratesio_api = to_string(&manifest_from_cratesio_api).unwrap();

        assert_eq!(
            serialized_manifest_from_sparse,
            serialized_manifest_from_cratesio_api
        );
    }

    #[cfg(feature = "git")]
    #[tokio::test]
    async fn test_crates_io_git_registry() {
        let client = create_client().await;

        let git_registry: Registry = "https://github.com/rust-lang/crates.io-index"
            .parse()
            .unwrap();
        assert!(
            matches!(git_registry, Registry::Git(_)),
            "{:?}",
            git_registry
        );

        let crate_name = "cargo-binstall";
        let version_req = &VersionReq::parse("=1.0.0").unwrap();
        let manifest_from_git = git_registry
            .fetch_crate_matched(client.clone(), crate_name, version_req)
            .await
            .unwrap();

        let manifest_from_cratesio_api = Registry::default()
            .fetch_crate_matched(client, crate_name, version_req)
            .await
            .unwrap();

        let serialized_manifest_from_git = to_string(&manifest_from_git).unwrap();
        let serialized_manifest_from_cratesio_api = to_string(&manifest_from_cratesio_api).unwrap();

        assert_eq!(
            serialized_manifest_from_git,
            serialized_manifest_from_cratesio_api
        );
    }
}
