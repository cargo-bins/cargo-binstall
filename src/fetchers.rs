use std::path::Path;
use std::sync::Arc;

pub use gh_crate_meta::*;
pub use log::debug;
pub use quickinstall::*;
use tokio::task::JoinHandle;

use crate::{BinstallError, PkgFmt, PkgMeta};

mod gh_crate_meta;
mod quickinstall;

#[async_trait::async_trait]
pub trait Fetcher: Send + Sync {
    /// Create a new fetcher from some data
    async fn new(data: &Data) -> Arc<Self>
    where
        Self: Sized;

    /// Fetch a package
    async fn fetch(&self, dst: &Path) -> Result<(), BinstallError>;

    /// Check if a package is available for download
    async fn check(&self) -> Result<bool, BinstallError>;

    /// Return the package format
    fn pkg_fmt(&self) -> PkgFmt;

    /// A short human-readable name or descriptor for the package source
    fn source_name(&self) -> String;

    /// Should return true if the remote is from a third-party source
    fn is_third_party(&self) -> bool;
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

#[derive(Default)]
pub struct MultiFetcher {
    fetchers: Vec<Arc<dyn Fetcher>>,
}

impl MultiFetcher {
    pub fn add(&mut self, fetcher: Arc<dyn Fetcher>) {
        self.fetchers.push(fetcher);
    }

    pub async fn first_available(&self) -> Option<Arc<dyn Fetcher>> {
        let handles: Vec<_> = self
            .fetchers
            .iter()
            .cloned()
            .map(|fetcher| {
                let fetcher_cloned = fetcher.clone();

                (
                    AutoAbortJoinHandle(tokio::spawn(async move {
                        fetcher.check().await.unwrap_or_else(|err| {
                            debug!(
                                "Error while checking fetcher {}: {}",
                                fetcher.source_name(),
                                err
                            );
                            false
                        })
                    })),
                    fetcher_cloned,
                )
            })
            .collect();

        for (mut handle, fetcher) in handles {
            match (&mut handle.0).await {
                Ok(true) => return Some(fetcher),
                Err(join_err) => {
                    debug!(
                        "Error while checking fetcher {}: {}",
                        fetcher.source_name(),
                        join_err
                    );
                }
                _ => (),
            }
        }

        None
    }
}

#[derive(Debug)]
struct AutoAbortJoinHandle(JoinHandle<bool>);

impl Drop for AutoAbortJoinHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}
