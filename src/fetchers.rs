use std::path::Path;

pub use gh_release::*;

use crate::PkgMeta;

mod gh_release;

#[async_trait::async_trait]
pub trait Fetcher {
    /// Create a new fetcher from some data
    async fn new(data: &Data) -> Result<Self, anyhow::Error>
    where
        Self: std::marker::Sized;

    /// Fetch a package
    async fn fetch(&self, dst: &Path) -> Result<(), anyhow::Error>;
    
    /// Check if a package is available for download
    async fn check(&self) -> Result<bool, anyhow::Error>;
}

/// Data required to fetch a package
pub struct Data {
    pub name: String,
    pub target: String,
    pub version: String,
    pub repo: Option<String>,
    pub meta: PkgMeta,
}