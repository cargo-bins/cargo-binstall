use std::path::Path;

use log::{debug, info};
use reqwest::Method;
use serde::Serialize;

use super::Data;
use crate::{download, remote_exists, PkgFmt, Template};

pub struct GhCrateMeta {
    url: String,
    pkg_fmt: PkgFmt,
}

#[async_trait::async_trait]
impl super::Fetcher for GhCrateMeta {
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

        Ok(Box::new(Self {
            url: ctx.render(&data.meta.pkg_url)?,
            pkg_fmt: data.meta.pkg_fmt,
        }))
    }

    async fn check(&self) -> Result<bool, anyhow::Error> {
        info!("Checking for package at: '{}'", self.url);
        remote_exists(&self.url, Method::HEAD).await
    }

    async fn fetch(&self, dst: &Path) -> Result<(), anyhow::Error> {
        info!("Downloading package from: '{}'", self.url);
        download(&self.url, dst).await
    }

    fn pkg_fmt(&self) -> PkgFmt {
        self.pkg_fmt
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
