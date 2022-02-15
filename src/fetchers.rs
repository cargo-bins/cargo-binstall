use std::path::Path;

pub use gh_release::*;
pub use quickinstall::*;

use crate::PkgMeta;

mod gh_release;
mod quickinstall;

#[async_trait::async_trait]
pub trait Fetcher {
    /// Create a new fetcher from some data
    async fn new(data: &Data) -> Result<Box<Self>, anyhow::Error> where Self: Sized;

    /// Fetch a package
    async fn fetch(&self, dst: &Path) -> Result<(), anyhow::Error>;
    
    /// Check if a package is available for download
    async fn check(&self) -> Result<bool, anyhow::Error>;
}

/// Data required to fetch a package
#[derive(Debug)]
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
            if fetcher.check().await.unwrap_or(false) {
                return Some(&**fetcher);
            }
        }
        
        None
    }
}