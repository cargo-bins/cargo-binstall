use std::{path::Path, sync::Arc};

use compact_str::CompactString;
use tracing::{debug, warn};
use url::Url;

use crate::{
    errors::BinstallError,
    helpers::{
        download::{Download, ExtractedFiles},
        gh_api_client::GhApiClient,
        remote::{does_url_exist, Client},
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
    tag: String,
    package: String,
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
        Arc::new(Self {
            client,
            gh_api_client,
            tag: format!("{crate_name}-{version}"),
            package: format!("{crate_name}-{version}-{target}"),
            target_data,
        })
    }

    fn find(self: Arc<Self>) -> AutoAbortJoinHandle<Result<bool, BinstallError>> {
        AutoAbortJoinHandle::spawn(async move {
            if cfg!(debug_assertions) {
                debug!("Not sending quickinstall report in debug mode");
            } else {
                let this = self.clone();
                tokio::spawn(async move {
                    if let Err(err) = this.report().await {
                        warn!(
                            "Failed to send quickinstall report for package {}: {err}",
                            this.package
                        )
                    }
                });
            }

            let url = self.package_url();
            does_url_exist(
                self.client.clone(),
                self.gh_api_client.clone(),
                &Url::parse(&url)?,
            )
            .await
        })
    }

    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, BinstallError> {
        let url = self.package_url();
        debug!("Downloading package from: '{url}'");
        Ok(Download::new(self.client.clone(), Url::parse(&url)?)
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
}

impl QuickInstall {
    fn package_url(&self) -> String {
        format!(
            "{base_url}/{tag}/{package}.tar.gz",
            base_url = BASE_URL,
            tag = self.tag,
            package = self.package,
        )
    }

    fn stats_url(&self) -> String {
        format!(
            "{stats_url}/{package}.tar.gz",
            stats_url = STATS_URL,
            package = self.package,
        )
    }

    pub async fn report(&self) -> Result<(), BinstallError> {
        let url = Url::parse(&self.stats_url())?;
        debug!("Sending installation report to quickinstall ({url})");

        self.client.remote_gettable(url).await?;

        Ok(())
    }
}
