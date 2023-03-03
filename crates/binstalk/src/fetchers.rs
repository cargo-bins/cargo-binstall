use std::{path::Path, sync::Arc};

use compact_str::CompactString;
pub use gh_crate_meta::*;
pub use quickinstall::*;
use tokio::sync::OnceCell;
use url::Url;

use crate::{
    errors::BinstallError,
    helpers::{
        download::ExtractedFiles, gh_api_client::GhApiClient, remote::Client,
        tasks::AutoAbortJoinHandle,
    },
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

pub(crate) mod gh_crate_meta;
pub(crate) mod quickinstall;

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// Create a new fetcher from some data
    #[allow(clippy::new_ret_no_self)]
    fn new(
        client: Client,
        gh_api_client: GhApiClient,
        data: Arc<Data>,
        target_data: Arc<TargetData>,
    ) -> Arc<dyn Fetcher>
    where
        Self: Sized;

    /// Fetch a package and extract
    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, BinstallError>;

    /// Find the package, if it is available for download
    ///
    /// This may look for multiple remote targets, but must write (using some form of interior
    /// mutability) the best one to the implementing struct in some way so `fetch_and_extract` can
    /// proceed without additional work.
    ///
    /// Must return `true` if a package is available, `false` if none is, and reserve errors to
    /// fatal conditions only.
    fn find(self: Arc<Self>) -> AutoAbortJoinHandle<Result<bool, BinstallError>>;

    /// Return the package format
    fn pkg_fmt(&self) -> PkgFmt;

    /// Return finalized target meta.
    fn target_meta(&self) -> PkgMeta;

    /// A short human-readable name or descriptor for the package source
    fn source_name(&self) -> CompactString;

    /// A short human-readable name, must contains only characters
    /// and numbers and it also must be unique.
    ///
    /// It is used to create a temporary dir where it is used for
    /// [`Fetcher::fetch_and_extract`].
    fn fetcher_name(&self) -> &'static str;

    /// Should return true if the remote is from a third-party source
    fn is_third_party(&self) -> bool;

    /// Return the target for this fetcher
    fn target(&self) -> &str;
}

/// Data required to fetch a package
#[derive(Clone, Debug)]
pub struct Data {
    name: CompactString,
    version: CompactString,
    repo: Option<String>,
    repo_final_url: OnceCell<Option<Url>>,
}

impl Data {
    pub fn new(name: CompactString, version: CompactString, repo: Option<String>) -> Self {
        Self {
            name,
            version,
            repo,
            repo_final_url: OnceCell::new(),
        }
    }

    async fn resolve_final_repo_url(&self, client: &Client) -> Result<&Option<Url>, BinstallError> {
        self.repo_final_url
            .get_or_try_init(move || {
                Box::pin(async move {
                    if let Some(repo) = self.repo.as_deref() {
                        Ok(Some(
                            client.get_redirected_final_url(Url::parse(repo)?).await?,
                        ))
                    } else {
                        Ok(None)
                    }
                })
            })
            .await
    }
}

/// Target specific data required to fetch a package
#[derive(Clone, Debug)]
pub struct TargetData {
    pub target: String,
    pub meta: PkgMeta,
}
