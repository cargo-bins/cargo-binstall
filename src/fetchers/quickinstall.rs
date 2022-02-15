use std::path::Path;

use log::info;
use reqwest::Method;

use crate::{download, remote_exists};
use super::Data;

pub struct QuickInstall {
    url: String,
}

#[async_trait::async_trait]
impl super::Fetcher for QuickInstall {
    async fn new(data: &Data) -> Result<Box<Self>, anyhow::Error> {
        let crate_name = &data.name;
        let version = &data.version;
        let target = &data.target;
        Ok(Box::new(Self { url: format!("https://github.com/alsuren/cargo-quickinstall/releases/download/{crate_name}-{version}-{target}/{crate_name}-{version}-{target}.tar.gz") }))
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