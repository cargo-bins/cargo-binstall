use std::path::Path;
use std::sync::Arc;

use log::{debug, info};
use reqwest::Method;
use tokio::task::JoinHandle;
use url::Url;

use super::Data;
use crate::{download, remote_exists, BinstallError, PkgFmt};

const BASE_URL: &str = "https://github.com/alsuren/cargo-quickinstall/releases/download";
const STATS_URL: &str = "https://warehouse-clerk-tmp.vercel.app/api/crate";
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

pub struct QuickInstall {
    package: String,
    target: String,
}

#[async_trait::async_trait]
impl super::Fetcher for QuickInstall {
    async fn new(data: &Data) -> Arc<Self> {
        let crate_name = &data.name;
        let version = &data.version;
        let target = data.target.clone();
        Arc::new(Self {
            package: format!("{crate_name}-{version}-{target}"),
            target,
        })
    }

    async fn check(&self) -> Result<bool, BinstallError> {
        let url = self.package_url();
        self.report();
        info!("Checking for package at: '{url}'");
        remote_exists(Url::parse(&url)?, Method::HEAD).await
    }

    async fn fetch(&self, dst: &Path) -> Result<(), BinstallError> {
        let url = self.package_url();
        info!("Downloading package from: '{url}'");
        download(&url, &dst).await
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

    fn target(&self) -> &str {
        &self.target
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

    pub fn report(&self) -> JoinHandle<Result<(), BinstallError>> {
        let stats_url = self.stats_url();

        tokio::spawn(async move {
            if cfg!(debug_assertions) {
                debug!("Not sending quickinstall report in debug mode");
                return Ok(());
            }

            let url = Url::parse(&stats_url)?;
            debug!("Sending installation report to quickinstall ({url})");

            reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .build()?
                .request(Method::HEAD, url.clone())
                .send()
                .await
                .map_err(|err| BinstallError::Http {
                    method: Method::HEAD,
                    url,
                    err,
                })?;

            Ok(())
        })
    }
}
