use std::path::Path;

use log::info;
use reqwest::Method;

use super::Data;
use crate::{download, remote_exists, PkgFmt};

const BASE_URL: &str = "https://github.com/alsuren/cargo-quickinstall/releases/download";
const STATS_URL: &str = "https://warehouse-clerk-tmp.vercel.app/api/crate";
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

pub struct QuickInstall {
    package: String,
}

#[async_trait::async_trait]
impl super::Fetcher for QuickInstall {
    async fn new(data: &Data) -> Result<Box<Self>, anyhow::Error> {
        let crate_name = &data.name;
        let version = &data.version;
        let target = &data.target;
        Ok(Box::new(Self {
            package: format!("{crate_name}-{version}-{target}"),
        }))
    }

    async fn check(&self) -> Result<bool, anyhow::Error> {
        let url = self.package_url();
        self.report().await?;
        info!("Checking for package at: '{url}'");
        remote_exists(&url, Method::HEAD).await
    }

    async fn fetch(&self, dst: &Path) -> Result<(), anyhow::Error> {
        let url = self.package_url();
        info!("Downloading package from: '{url}'");
        download(&url, dst).await
    }

    fn pkg_fmt(&self) -> PkgFmt {
        PkgFmt::Tgz
    }

    fn source_name(&self) -> String {
        String::from("QuickInstall")
    }
    fn is_third_party(&self) -> bool {
        true
    }
}

impl QuickInstall {
    fn package_url(&self) -> String {
        format!(
            "{base_url}/{package}/{package}.tar.gz",
            base_url = BASE_URL,
            package = self.package
        )
    }

    fn stats_url(&self) -> String {
        format!(
            "{stats_url}/{package}.tar.gz",
            stats_url = STATS_URL,
            package = self.package
        )
    }

    pub async fn report(&self) -> Result<(), anyhow::Error> {
        info!("Sending installation report to quickinstall (anonymous)");
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()?
            .request(Method::HEAD, &self.stats_url())
            .send()
            .await?;
        Ok(())
    }
}
