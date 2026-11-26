use std::{borrow::Cow, path::Path, sync::Arc};

use binstalk_downloader::remote::Method;
use binstalk_types::cargo_toml_binstall::{PkgFmt, PkgMeta, PkgSigning, Strategy};
use tokio::sync::OnceCell;
use tracing::{error, info, trace};
use url::Url;

use crate::{
    Data, Extracted, FetchError, Found, SignaturePolicy, SignatureVerifier, SigningAlgorithm,
    TargetDataErased, common::*,
};

const BASE_URL: &str = "https://github.com/cargo-bins/cargo-quickinstall/releases/download";
pub const QUICKINSTALL_STATS_URL: &str =
    "https://cargo-quickinstall-stats-server.fly.dev/record-install";

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
    is_supported_v: OnceCell<bool>,

    data: Arc<Data>,
    package: String,
    package_url: Url,
    signature_url: Url,
    signature_policy: SignaturePolicy,

    target_data: Arc<TargetDataErased>,

    /// Send [`Status`] to quickinstall's stats server.
    telemetry: bool,
}

#[derive(Debug, Clone, Copy)]
enum Status {
    Start,
    NotFound,
    AttemptingInstall,
    InvalidSignature,
    InstalledFromTarball,
}

impl Status {
    fn as_str(&self) -> &'static str {
        match self {
            Status::Start => "start",
            Status::NotFound => "not-found",
            Status::AttemptingInstall => "attempting-install",
            Status::InvalidSignature => "invalid-signature",
            Status::InstalledFromTarball => "installed-from-tarball",
        }
    }
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

    fn download_signature(
        self: Arc<Self>,
    ) -> AutoAbortJoinHandle<Result<SignatureVerifier, FetchError>> {
        AutoAbortJoinHandle::spawn(async move {
            if self.signature_policy == SignaturePolicy::Ignore {
                Ok(SignatureVerifier::Noop)
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
                        let verifier = SignatureVerifier::new(&config, &signature)?;
                        self.check_signed_file_name(&verifier)?;
                        Ok(verifier)
                    }
                    Err(err) => {
                        if self.signature_policy == SignaturePolicy::Require {
                            error!("Failed to download signature: {err}");
                            Err(FetchError::MissingSignature)
                        } else {
                            debug!("Failed to download signature, skipping verification: {err}");
                            Ok(SignatureVerifier::Noop)
                        }
                    }
                }
            }
        })
    }

    /// The signature only covers the file's contents, so a valid signature
    /// for another crate, version or target would verify too. quickinstall
    /// signs with `minisign -S`, which puts the file name in the trusted
    /// comment, and that comment is covered by the signature: check it.
    ///
    /// The comment is only authenticated once the download is verified,
    /// which happens before the package is used.
    fn check_signed_file_name(&self, verifier: &SignatureVerifier) -> Result<(), FetchError> {
        let expected = format!("{}.tar.gz", self.package);
        let signed = verifier.trusted_comment().and_then(signed_file_name);

        if signed == Some(expected.as_str()) {
            Ok(())
        } else {
            error!(
                expected,
                signed = signed.unwrap_or("<none>"),
                "Signature is for a different file"
            );
            Err(FetchError::InvalidSignature)
        }
    }

    async fn find_package(self: Arc<Self>) -> Result<Option<Box<dyn Found>>, FetchError> {
        if !self.is_supported().await? {
            return Ok(None);
        }

        let download_signature_task = self.clone().download_signature();

        debug!("Checking for package at: '{}'", self.package_url);
        let Some(response) = get_if_exists(&self.client, &self.package_url).await? else {
            return Ok(None);
        };
        let verifier = download_signature_task.flattened_join().await?;

        Ok(Some(Box::new(QuickInstallFound {
            fetcher: self,
            verifier,
            response: Some(response),
        }) as Box<dyn Found>))
    }

    /// Sends `status` to quickinstall's stats server in the background.
    fn send_report(self: Arc<Self>, status: Status) {
        if !self.telemetry {
            return;
        }

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
                if let Err(err) = self.report(status).await {
                    warn!(
                        "Failed to send quickinstall report for package {} (NOTE that this does not affect package resolution): {err}",
                        self.package
                    )
                }
            });
        }
    }
}

#[async_trait::async_trait]
impl super::Fetcher for QuickInstall {
    fn new(
        client: Client,
        _gh_api_client: GhApiClient,
        data: Arc<Data>,
        target_data: Arc<TargetDataErased>,
        signature_policy: SignaturePolicy,
        telemetry: bool,
    ) -> Arc<dyn super::Fetcher> {
        let crate_name = &data.name;
        let version = &data.version;
        let target = &target_data.target;

        let package = format!("{crate_name}-{version}-{target}");

        let url = format!("{BASE_URL}/{crate_name}-{version}/{package}.tar.gz");

        Arc::new(Self {
            client,
            data,
            is_supported_v: OnceCell::new(),

            package_url: Url::parse(&url)
                .expect("package_url is pre-generated and should never be invalid url"),
            signature_url: Url::parse(&format!("{url}.sig"))
                .expect("signature_url is pre-generated and should never be invalid url"),
            package,
            signature_policy,

            target_data,
            telemetry,
        })
    }

    async fn find(self: Arc<Self>) -> Result<Option<Box<dyn Found>>, FetchError> {
        let ret = self.clone().find_package().await;

        let status = match &ret {
            // Reported by `QuickInstallFound::fetch_and_extract`.
            Ok(Some(_)) => None,
            Ok(None) => Some(Status::NotFound),
            Err(FetchError::InvalidSignature) => Some(Status::InvalidSignature),
            Err(_) => Some(Status::Start),
        };
        if let Some(status) = status {
            self.send_report(status);
        }

        ret
    }

    fn fetcher_name(&self) -> &'static str {
        "QuickInstall"
    }

    fn strategy(&self) -> Strategy {
        Strategy::QuickInstall
    }
}

/// The package [`QuickInstall`] found.
struct QuickInstallFound {
    fetcher: Arc<QuickInstall>,
    verifier: SignatureVerifier,
    /// The response to the GET that found the package, its body unread,
    /// which `fetch_and_extract` downloads instead of asking again.
    response: Option<GetResponse>,
}

impl QuickInstallFound {
    async fn download_and_extract(
        &self,
        response: Option<GetResponse>,
        dst: &Path,
    ) -> Result<ExtractedFiles, FetchError> {
        let fetcher = &self.fetcher;

        debug!(url=%fetcher.package_url, "Downloading package");
        let mut data_verifier = self.verifier.data_verifier()?;
        let files = response
            .map(Download::from)
            .unwrap_or_else(|| Download::new(fetcher.client.clone(), fetcher.package_url.clone()))
            .with_data_verifier(data_verifier.as_mut())
            .and_extract(self.pkg_fmt(), dst)
            .await?;
        trace!("validating signature (if any)");
        if data_verifier.validate() {
            if let Some(info) = self.verifier.info() {
                info!(
                    "Verified signature for package '{}': {info}",
                    fetcher.package
                );
            }
            Ok(files)
        } else {
            Err(FetchError::InvalidSignature)
        }
    }
}

#[async_trait::async_trait]
impl Found for QuickInstallFound {
    async fn fetch_and_extract(&mut self, dst: &Path) -> Result<Arc<Extracted>, FetchError> {
        let response = self.response.take();
        // Its URLs are per crate, so it is never shared with other crates.
        let ret = self
            .download_and_extract(response, dst)
            .await
            .map(|files| Arc::new(Extracted::new(dst.to_path_buf(), files)));

        self.fetcher.clone().send_report(match &ret {
            Ok(_) => Status::InstalledFromTarball,
            Err(FetchError::InvalidSignature) => Status::InvalidSignature,
            Err(_) => Status::AttemptingInstall,
        });

        ret
    }

    fn pkg_fmt(&self) -> PkgFmt {
        PkgFmt::Tgz
    }

    fn target_meta(&self) -> PkgMeta {
        let mut meta = self.fetcher.target_data.meta.clone();
        meta.pkg_fmt = Some(self.pkg_fmt());
        meta.bin_dir = Some("{ bin }{ binary-ext }".to_string());
        meta
    }

    fn is_third_party(&self) -> bool {
        true
    }

    fn target(&self) -> &str {
        &self.fetcher.target_data.target
    }

    fn target_data(&self) -> &Arc<TargetDataErased> {
        &self.fetcher.target_data
    }

    fn source_name(&self) -> CompactString {
        CompactString::from("QuickInstall")
    }
}

impl QuickInstall {
    async fn report(&self, status: Status) -> Result<(), FetchError> {
        if !self.is_supported().await? {
            debug!(
                "Not sending quickinstall report for {} since Quickinstall does not support these targets.",
                self.target_data.target
            );

            return Ok(());
        }

        let mut url = Url::parse(QUICKINSTALL_STATS_URL)
            .expect("stats_url is pre-generated and should never be invalid url");
        url.query_pairs_mut()
            .append_pair("crate", &self.data.name)
            .append_pair("version", &self.data.version)
            .append_pair("target", &self.target_data.target)
            .append_pair(
                "agent",
                concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            )
            .append_pair("status", status.as_str());
        debug!("Sending installation report to quickinstall ({url})");

        self.client.request(Method::POST, url).send(true).await?;

        Ok(())
    }
}

/// Returns the `file:` field of a trusted comment written by `minisign -S`,
/// such as `timestamp:1757327413\tfile:ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz\thashed`.
fn signed_file_name(trusted_comment: &str) -> Option<&str> {
    trusted_comment
        .split('\t')
        .find_map(|field| field.strip_prefix("file:"))
}

#[cfg(test)]
mod test {
    use super::{Client, CompactString, get_quickinstall_supported_targets, signed_file_name};
    use std::num::NonZeroU16;

    /// Mark this as an async fn so that you won't accidentally use it in
    /// sync context.
    async fn create_client() -> Client {
        Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
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

    #[test]
    fn test_signed_file_name() {
        assert_eq!(
            signed_file_name(
                "timestamp:1757327413\tfile:ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz\thashed"
            ),
            Some("ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            signed_file_name("timestamp:1555779966\tfile:test"),
            Some("test")
        );
        assert_eq!(signed_file_name("timestamp:1555779966"), None);
        assert_eq!(signed_file_name("my file:x.tar.gz"), None);
    }
}
