use std::{borrow::Cow, path::Path, sync::Arc};

use binstalk_downloader::remote::Method;
use binstalk_types::cargo_toml_binstall::{PkgFmt, PkgMeta, PkgSigning};
use tokio::sync::OnceCell;
use tracing::{error, info, trace};
use url::Url;

use crate::{
    common::*, Data, FetchError, SignaturePolicy, SignatureVerifier, SigningAlgorithm,
    TargetDataErased,
};

const BASE_URL: &str = "https://github.com/cargo-bins/cargo-quickinstall/releases/download";
const STATS_URL: &str = "https://warehouse-clerk-tmp.vercel.app/api/crate";

const QUICKINSTALL_SIGN_KEY: Cow<'static, str> =
    Cow::Borrowed("RWTdnnab2pAka9OdwgCMYyOE66M/BlQoFWaJ/JjwcPV+f3n24IRTj97t");
const QUICKINSTALL_SUPPORTED_TARGETS_URL: &str =
    "https://raw.githubusercontent.com/cargo-bins/cargo-quickinstall/main/supported-targets";

fn is_universal_macos(target: &str) -> bool {
    ["universal-apple-darwin", "universal2-apple-darwin"].contains(&target)
}

async fn get_quickinstall_supported_targets(
    client: &Client,
) -> Result<&'static [CompactString], FetchError> {
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
    signature_url: Url,
    stats_url: Url,
    signature_policy: SignaturePolicy,

    target_data: Arc<TargetDataErased>,
}

impl QuickInstall {
    async fn is_supported(&self) -> Result<bool, FetchError> {
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
        _cacher: HTTPCacher,
        data: Arc<Data>,
        target_data: Arc<TargetDataErased>,
        signature_policy: SignaturePolicy,
    ) -> Arc<dyn super::Fetcher> {
        let crate_name = &data.name;
        let version = &data.version;
        let target = &target_data.target;

        let package = format!("{crate_name}-{version}-{target}");

        let url = format!("{BASE_URL}/{crate_name}-{version}/{package}.tar.gz");

        Arc::new(Self {
            client,
            gh_api_client,
            is_supported_v: OnceCell::new(),

            package_url: Url::parse(&url)
                .expect("package_url is pre-generated and should never be invalid url"),
            signature_url: Url::parse(&format!("{url}.sig"))
                .expect("signature_url is pre-generated and should never be invalid url"),
            stats_url: Url::parse(&format!("{STATS_URL}/{package}.tar.gz",))
                .expect("stats_url is pre-generated and should never be invalid url"),
            package,
            signature_policy,

            target_data,
        })
    }

    fn find(self: Arc<Self>) -> JoinHandle<Result<bool, FetchError>> {
        tokio::spawn(async move {
            if !self.is_supported().await? {
                return Ok(false);
            }

            if self.signature_policy == SignaturePolicy::Require {
                does_url_exist(
                    self.client.clone(),
                    self.gh_api_client.clone(),
                    &self.signature_url,
                )
                .await
                .map_err(|_| FetchError::MissingSignature)?;
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

    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, FetchError> {
        let verifier = if self.signature_policy == SignaturePolicy::Ignore {
            SignatureVerifier::Noop
        } else {
            debug!(url=%self.signature_url, "Downloading signature");
            match Download::new(self.client.clone(), self.signature_url.clone())
                .into_bytes()
                .await
            {
                Ok(signature) => {
                    trace!(?signature, "got signature contents");
                    let config = PkgSigning {
                        algorithm: SigningAlgorithm::Minisign,
                        pubkey: QUICKINSTALL_SIGN_KEY,
                        file: None,
                    };
                    SignatureVerifier::new(&config, &signature)?
                }
                Err(err) => {
                    if self.signature_policy == SignaturePolicy::Require {
                        error!("Failed to download signature: {err}");
                        return Err(FetchError::MissingSignature);
                    }

                    debug!("Failed to download signature, skipping verification: {err}");
                    SignatureVerifier::Noop
                }
            }
        };

        debug!(url=%self.package_url, "Downloading package");
        let mut data_verifier = verifier.data_verifier()?;
        let files = Download::new_with_data_verifier(
            self.client.clone(),
            self.package_url.clone(),
            data_verifier.as_mut(),
        )
        .and_extract(self.pkg_fmt(), dst)
        .await?;
        trace!("validating signature (if any)");
        if data_verifier.validate() {
            if let Some(info) = verifier.info() {
                info!("Verified signature for package '{}': {info}", self.package);
            }
            Ok(files)
        } else {
            Err(FetchError::InvalidSignature)
        }
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

    fn target_data(&self) -> &Arc<TargetDataErased> {
        &self.target_data
    }
}

impl QuickInstall {
    pub async fn report(&self) -> Result<(), FetchError> {
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
    use std::num::NonZeroU16;

    /// Mark this as an async fn so that you won't accidentally use it in
    /// sync context.
    async fn create_client() -> Client {
        Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            NonZeroU16::new(10).unwrap(),
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
