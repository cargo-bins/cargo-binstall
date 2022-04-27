use std::path::Path;

pub use gh_crate_meta::*;
pub use log::debug;
pub use quickinstall::*;

use crate::{PkgFmt, PkgMeta};

mod gh_crate_meta;
mod quickinstall;

#[async_trait::async_trait]
pub trait Fetcher {
    /// Create a new fetcher from some data
    async fn new(data: &Data) -> Box<Self>
    where
        Self: Sized;

    /// Fetch a package
    async fn fetch(&self, dst: &Path) -> Result<(), anyhow::Error>;

    /// Check if a package is available for download
    async fn check(&self) -> Result<bool, anyhow::Error>;

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
    fetchers: Vec<Box<dyn Fetcher>>,
}

impl MultiFetcher {
    pub fn add(&mut self, fetcher: Box<dyn Fetcher>) {
        self.fetchers.push(fetcher);
    }

    pub async fn first_available(&self) -> Option<&dyn Fetcher> {
        for fetcher in &self.fetchers {
            if fetcher.check().await.unwrap_or_else(|err| {
                debug!(
                    "Error while checking fetcher {}: {}",
                    fetcher.source_name(),
                    err
                );
                false
            }) {
                return Some(&**fetcher);
            }
        }

        None
    }
}
