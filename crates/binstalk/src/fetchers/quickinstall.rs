use std::{path::Path, sync::Arc};

use compact_str::CompactString;
use tokio::sync::OnceCell;
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

const QUICKINSTALL_SUPPORTED_TARGETS_URL: &str =
    "https://raw.githubusercontent.com/cargo-bins/cargo-quickinstall/main/supported-targets";

async fn get_quickinstall_supported_targets(
    client: &Client,
) -> Result<&'static [CompactString], BinstallError> {
    static SUPPORTED_TARGETS: OnceCell<Box<[CompactString]>> = OnceCell::const_new();

    SUPPORTED_TARGETS
        .get_or_try_init(|| async {
            let bytes = client
                .get(Url::parse(QUICKINSTALL_SUPPORTED_TARGETS_URL)?)
                .send(true)
                .await?
                .bytes()
                .await?;

            let mut v: Vec<CompactString> = String::from_utf8_lossy(&bytes)
                .split_whitespace()
                .map(CompactString::new)
                .collect();
            v.sort_unstable();
            v.dedup();
            Ok(v.into())
        })
        .await
        .map(Box::as_ref)
}

pub struct QuickInstall {
    client: Client,
    gh_api_client: GhApiClient,
    is_supported_v: OnceCell<bool>,

    package: String,
    package_url: Url,
    stats_url: Url,

    target_data: Arc<TargetData>,
}

impl QuickInstall {
    async fn is_supported(&self) -> Result<bool, BinstallError> {
        self.is_supported_v
            .get_or_try_init(|| async {
                Ok(get_quickinstall_supported_targets(&self.client)
                    .await?
                    .binary_search(&CompactString::new(&self.target_data.target))
                    .is_ok())
            })
            .await
            .copied()
    }
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
            is_supported_v: OnceCell::new(),

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
            if !self.is_supported().await? {
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
        } else if self.is_supported_v.get().copied() != Some(false) {
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
        if !self.is_supported().await? {
            debug!(
                "Not sending quickinstall report for {} since Quickinstall does not support these targets.",
                self.target_data.target
            );

            return Ok(());
        }

        let url = self.stats_url.clone();
        debug!("Sending installation report to quickinstall ({url})");

        self.client.request(Method::HEAD, url).send(true).await?;

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::{get_quickinstall_supported_targets, Client, CompactString};
    use std::time::Duration;

    /// Mark this as an async fn so that you won't accidentally use it in
    /// sync context.
    async fn create_client() -> Client {
        Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            Duration::from_millis(10),
            1.try_into().unwrap(),
            [],
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_get_quickinstall_supported_targets() {
        let supported_targets = get_quickinstall_supported_targets(&create_client().await)
            .await
            .unwrap();

        [
            "x86_64-pc-windows-msvc",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-gnu",
            "aarch64-unknown-linux-musl",
            "aarch64-pc-windows-msvc",
            "armv7-unknown-linux-musleabihf",
            "armv7-unknown-linux-gnueabihf",
        ]
        .into_iter()
        .for_each(|known_supported_target| {
            supported_targets
                .binary_search(&CompactString::new(known_supported_target))
                .unwrap();
        });
    }
}
