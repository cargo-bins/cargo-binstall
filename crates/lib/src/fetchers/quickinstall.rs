use std::{path::Path, sync::Arc};

use compact_str::CompactString;
use log::debug;
use reqwest::Client;
use reqwest::Method;
use tokio::task::JoinHandle;
use url::Url;

use crate::{
    errors::BinstallError,
    helpers::{download::Download, remote::remote_exists},
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

use super::Data;

const BASE_URL: &str = "https://github.com/alsuren/cargo-quickinstall/releases/download";
const STATS_URL: &str = "https://warehouse-clerk-tmp.vercel.app/api/crate";

pub struct QuickInstall {
    client: Client,
    package: String,
    target: String,
    data: Arc<Data>,
}

#[async_trait::async_trait]
impl super::Fetcher for QuickInstall {
    async fn new(client: &Client, data: &Arc<Data>) -> Arc<Self> {
        let crate_name = &data.name;
        let version = &data.version;
        let target = data.target.clone();
        Arc::new(Self {
            client: client.clone(),
            package: format!("{crate_name}-{version}-{target}"),
            target,
            data: data.clone(),
        })
    }

    async fn find(&self) -> Result<bool, BinstallError> {
        let url = self.package_url();
        self.report();
        debug!("Checking for package at: '{url}'");
        remote_exists(self.client.clone(), Url::parse(&url)?, Method::HEAD).await
    }

    async fn fetch_and_extract(&self, dst: &Path) -> Result<(), BinstallError> {
        let url = self.package_url();
        debug!("Downloading package from: '{url}'");
        Download::new(&self.client, Url::parse(&url)?)
            .and_extract(self.pkg_fmt(), dst)
            .await
    }

    fn pkg_fmt(&self) -> PkgFmt {
        PkgFmt::Tgz
    }

    fn target_meta(&self) -> PkgMeta {
        let mut meta = self.data.meta.clone();
        meta.pkg_fmt = Some(self.pkg_fmt());
        meta
    }

    fn source_name(&self) -> CompactString {
        CompactString::from("QuickInstall")
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
        let client = self.client.clone();

        tokio::spawn(async move {
            if cfg!(debug_assertions) {
                debug!("Not sending quickinstall report in debug mode");
                return Ok(());
            }

            let url = Url::parse(&stats_url)?;
            debug!("Sending installation report to quickinstall ({url})");

            client
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
