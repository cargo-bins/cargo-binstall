use std::path::Path;

use log::{debug, info};
use reqwest::Method;
use serde::Serialize;

use crate::{download, remote_exists, Template};
use super::Data;

pub struct GhRelease {
    url: String,
}

#[async_trait::async_trait]
impl super::Fetcher for GhRelease {
    async fn new(data: &Data) -> Result<Box<Self>, anyhow::Error> {
        // Generate context for URL interpolation
        let ctx = Context { 
            name: &data.name,
            repo: data.repo.as_ref().map(|s| &s[..]),
            target: &data.target, 
            version: &data.version,
            format: data.meta.pkg_fmt.to_string(),
        };
        debug!("Using context: {:?}", ctx);

        Ok(Box::new(Self { url: ctx.render(&data.meta.pkg_url)? }))
    }

    async fn check(&self) -> Result<bool, anyhow::Error> {
        info!("Checking for package at: '{}'", self.url);
        remote_exists(&self.url, Method::OPTIONS).await
    }

    async fn fetch(&self, dst: &Path) -> Result<(), anyhow::Error> {
        info!("Downloading package from: '{}'", self.url);
        download(&self.url, dst).await
    }
}

/// Template for constructing download paths
#[derive(Clone, Debug, Serialize)]
struct Context<'c> {
    pub name: &'c str,
    pub repo: Option<&'c str>,
    pub target: &'c str,
    pub version: &'c str,
    pub format: String,
}

impl<'c> Template for Context<'c> {}