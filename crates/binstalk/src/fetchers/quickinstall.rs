use std::{path::Path, sync::Arc};

use compact_str::CompactString;
use tracing::{debug, warn};
use url::Url;

use crate::{
    errors::BinstallError,
    helpers::{
        download::{Download, ExtractedFiles},
        gh_api_client::GhApiClient,
        is_universal_macos,
        remote::{does_url_exist, Client, Method},
        tasks::AutoAbortJoinHandle,
    },
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

use super::{Data, TargetData};

const BASE_URL: &str = "https://github.com/cargo-bins/cargo-quickinstall/releases/download";
const STATS_URL: &str = "https://warehouse-clerk-tmp.vercel.app/api/crate";

pub struct QuickInstall {
    client: Client,
    gh_api_client: GhApiClient,

    package: String,
    package_url: Url,
    stats_url: Url,

    target_data: Arc<TargetData>,
}

#[async_trait::async_trait]
impl super::Fetcher for QuickInstall {
    fn new(
        client: Client,
        gh_api_client: GhApiClient,
        data: Arc<Data>,
        target_data: Arc<TargetData>,
    ) -> Arc<dyn super::Fetcher> {
        let crate_name = &data.name;
        let version = &data.version;
        let target = &target_data.target;

        let package = format!("{crate_name}-{version}-{target}");

        Arc::new(Self {
            client,
            gh_api_client,

            package_url: Url::parse(&format!(
                "{BASE_URL}/{crate_name}-{version}/{package}.tar.gz",
            ))
            .expect("package_url is pre-generated and should never be invalid url"),
            stats_url: Url::parse(&format!("{STATS_URL}/{package}.tar.gz",))
                .expect("stats_url is pre-generated and should never be invalid url"),
            package,

            target_data,
        })
    }

    fn find(self: Arc<Self>) -> AutoAbortJoinHandle<Result<bool, BinstallError>> {
        AutoAbortJoinHandle::spawn(async move {
            if is_universal_macos(&self.target_data.target) {
                return Ok(false);
            }

            does_url_exist(
                self.client.clone(),
                self.gh_api_client.clone(),
                &self.package_url,
            )
            .await
        })
    }

    fn report_to_upstream(self: Arc<Self>) {
        if cfg!(debug_assertions) {
            debug!("Not sending quickinstall report in debug mode");
        } else if is_universal_macos(&self.target_data.target) {
            debug!(
                r#"Not sending quickinstall report for universal-apple-darwin
and universal2-apple-darwin.
Quickinstall does not support these targets, it only supports targets supported
by rust officially."#,
            );
        } else {
            tokio::spawn(async move {
                if let Err(err) = self.report().await {
                    warn!(
                        "Failed to send quickinstall report for package {}: {err}",
                        self.package
                    )
                }
            });
        }
    }

    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, BinstallError> {
        let url = &self.package_url;
        debug!("Downloading package from: '{url}'");
        Ok(Download::new(self.client.clone(), url.clone())
            .and_extract(self.pkg_fmt(), dst)
            .await?)
    }

    fn pkg_fmt(&self) -> PkgFmt {
        PkgFmt::Tgz
    }

    fn target_meta(&self) -> PkgMeta {
        let mut meta = self.target_data.meta.clone();
        meta.pkg_fmt = Some(self.pkg_fmt());
        meta.bin_dir = Some("{ bin }{ binary-ext }".to_string());
        meta
    }

    fn source_name(&self) -> CompactString {
        CompactString::from("QuickInstall")
    }

    fn fetcher_name(&self) -> &'static str {
        "QuickInstall"
    }

    fn is_third_party(&self) -> bool {
        true
    }

    fn target(&self) -> &str {
        &self.target_data.target
    }

    fn target_data(&self) -> &Arc<TargetData> {
        &self.target_data
    }
}

impl QuickInstall {
    pub async fn report(&self) -> Result<(), BinstallError> {
        let url = self.stats_url.clone();
        debug!("Sending installation report to quickinstall ({url})");

        self.client.request(Method::HEAD, url).send(true).await?;

        Ok(())
    }
}
