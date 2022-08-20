use std::{path::Path, sync::Arc};

use compact_str::CompactString;
pub use gh_crate_meta::*;
pub use log::debug;
pub use quickinstall::*;
use reqwest::Client;

use crate::{
    errors::BinstallError,
    helpers::tasks::AutoAbortJoinHandle,
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

mod gh_crate_meta;
mod quickinstall;

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// Create a new fetcher from some data
    async fn new(client: &Client, data: &Data) -> Arc<Self>
    where
        Self: Sized;

    /// Fetch a package and extract
    async fn fetch_and_extract(&self, dst: &Path) -> Result<(), BinstallError>;

    /// Find the package, if it is available for download
    ///
    /// This may look for multiple remote targets, but must write (using some form of interior
    /// mutability) the best one to the implementing struct in some way so `fetch_and_extract` can
    /// proceed without additional work.
    ///
    /// Must return `true` if a package is available, `false` if none is, and reserve errors to
    /// fatal conditions only.
    async fn find(&self) -> Result<bool, BinstallError>;

    /// Return the package format
    fn pkg_fmt(&self) -> PkgFmt;

    /// A short human-readable name or descriptor for the package source
    fn source_name(&self) -> CompactString;

    /// Should return true if the remote is from a third-party source
    fn is_third_party(&self) -> bool;

    /// Return the target for this fetcher
    fn target(&self) -> &str;
}

/// Data required to fetch a package
#[derive(Clone, Debug)]
pub struct Data {
    pub name: String,
    pub target: String,
    pub version: String,
    pub repo: Option<String>,
    pub meta: PkgMeta,
}

type FetcherJoinHandle = AutoAbortJoinHandle<Result<bool, BinstallError>>;

#[derive(Default)]
pub struct MultiFetcher(Vec<(Arc<dyn Fetcher>, FetcherJoinHandle)>);

impl MultiFetcher {
    pub fn add(&mut self, fetcher: Arc<dyn Fetcher>) {
        self.0.push((
            fetcher.clone(),
            AutoAbortJoinHandle::spawn(async move { fetcher.find().await }),
        ));
    }

    pub async fn first_available(self) -> Option<Arc<dyn Fetcher>> {
        for (fetcher, handle) in self.0 {
            match handle.await {
                Ok(Ok(true)) => return Some(fetcher),
                Ok(Ok(false)) => (),
                Ok(Err(err)) => {
                    debug!(
                        "Error while checking fetcher {}: {}",
                        fetcher.source_name(),
                        err
                    );
                }
                Err(join_err) => {
                    debug!(
                        "Error while joining the task that checks the fetcher {}: {}",
                        fetcher.source_name(),
                        join_err
                    );
                }
            }
        }

        None
    }
}
