use std::{path::Path, sync::Arc};

use compact_str::{CompactString, ToCompactString};
use log::{debug, warn};
use once_cell::sync::OnceCell;
use reqwest::{Client, Method};
use serde::Serialize;
use tinytemplate::TinyTemplate;
use url::Url;

use crate::{
    errors::BinstallError,
    helpers::{download::Download, remote::remote_exists, tasks::AutoAbortJoinHandle},
    manifests::cargo_toml_binstall::PkgFmt,
};

use super::Data;

pub struct GhCrateMeta {
    client: Client,
    data: Data,
    url: OnceCell<Url>,
}

#[async_trait::async_trait]
impl super::Fetcher for GhCrateMeta {
    async fn new(client: &Client, data: &Data) -> Arc<Self> {
        Arc::new(Self {
            client: client.clone(),
            data: data.clone(),
            url: OnceCell::new(),
        })
    }

    async fn find(&self) -> Result<bool, BinstallError> {
        // build up list of potential URLs
        let urls = self.data.meta.pkg_fmt.extensions().iter().map(|ext| {
            let ctx = Context::from_data(&self.data, ext);
            ctx.render_url(&self.data.meta.pkg_url)
        });

        // go check all potential URLs at once
        let checks = urls
            .map(|url| {
                let client = self.client.clone();
                AutoAbortJoinHandle::spawn(async move {
                    let url = url?;
                    debug!("Checking for package at: '{url}'");
                    remote_exists(client, url.clone(), Method::HEAD)
                        .await
                        .map(|exists| (url.clone(), exists))
                })
            })
            .collect::<Vec<_>>();

        // get the first URL that exists
        for check in checks {
            let (url, exists) = check.await??;
            if exists {
                if url.scheme() != "https" {
                    warn!(
                        "URL is not HTTPS! This may become a hard error in the future, tell the upstream!"
                    );
                }

                debug!("Winning URL is {url}");
                self.url.set(url).unwrap(); // find() is called first
                return Ok(true);
            }
        }

        Ok(false)
    }

    async fn fetch_and_extract(&self, dst: &Path) -> Result<(), BinstallError> {
        let url = self.url.get().unwrap(); // find() is called first
        debug!("Downloading package from: '{url}'");
        Download::new(&self.client, url.clone()).and_extract(self.pkg_fmt(), dst).await
    }

    fn pkg_fmt(&self) -> PkgFmt {
        self.data.meta.pkg_fmt
    }

    fn source_name(&self) -> CompactString {
        self.url
            .get()
            .map(|url| {
                if let Some(domain) = url.domain() {
                    domain.to_compact_string()
                } else if let Some(host) = url.host_str() {
                    host.to_compact_string()
                } else {
                    url.to_compact_string()
                }
            })
            .unwrap_or_else(|| "invalid url".into())
    }

    fn is_third_party(&self) -> bool {
        false
    }

    fn target(&self) -> &str {
        &self.data.target
    }
}

/// Template for constructing download paths
#[derive(Clone, Debug, Serialize)]
struct Context<'c> {
    pub name: &'c str,
    pub repo: Option<&'c str>,
    pub target: &'c str,
    pub version: &'c str,

    /// Soft-deprecated alias for archive-format
    pub format: &'c str,

    /// Archive format e.g. tar.gz, zip
    #[serde(rename = "archive-format")]
    pub archive_format: &'c str,

    /// Filename extension on the binary, i.e. .exe on Windows, nothing otherwise
    #[serde(rename = "binary-ext")]
    pub binary_ext: &'c str,
}

impl<'c> Context<'c> {
    pub(self) fn from_data(data: &'c Data, archive_format: &'c str) -> Self {
        Self {
            name: &data.name,
            repo: data.repo.as_ref().map(|s| &s[..]),
            target: &data.target,
            version: &data.version,
            format: archive_format,
            archive_format,
            binary_ext: if data.target.contains("windows") {
                ".exe"
            } else {
                ""
            },
        }
    }

    pub(self) fn render_url(&self, template: &str) -> Result<Url, BinstallError> {
        debug!("Render {template:?} using context: {:?}", self);

        let mut tt = TinyTemplate::new();
        tt.add_template("path", template)?;
        Ok(Url::parse(&tt.render("path", self)?)?)
    }
}

#[cfg(test)]
mod test {
    use crate::manifests::cargo_toml_binstall::{PkgFmt, PkgMeta};

    use super::{super::Data, Context};
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn defaults() {
        let meta = PkgMeta::default();
        let data = Data {
            name: "cargo-binstall".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            version: "1.2.3".to_string(),
            repo: Some("https://github.com/ryankurte/cargo-binstall".to_string()),
            meta,
        };

        let ctx = Context::from_data(&data, "tgz");
        assert_eq!(
            ctx.render_url(&data.meta.pkg_url).unwrap(),
            url("https://github.com/ryankurte/cargo-binstall/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz")
        );
    }

    #[test]
    #[should_panic]
    fn no_repo() {
        let meta = PkgMeta::default();
        let data = Data {
            name: "cargo-binstall".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            version: "1.2.3".to_string(),
            repo: None,
            meta,
        };

        let ctx = Context::from_data(&data, "tgz");
        ctx.render_url(&data.meta.pkg_url).unwrap();
    }

    #[test]
    fn no_repo_but_full_url() {
        let meta = PkgMeta {
            pkg_url: format!("https://example.com{}", PkgMeta::default().pkg_url),
            ..Default::default()
        };

        let data = Data {
            name: "cargo-binstall".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            version: "1.2.3".to_string(),
            repo: None,
            meta,
        };

        let ctx = Context::from_data(&data, "tgz");
        assert_eq!(
            ctx.render_url(&data.meta.pkg_url).unwrap(),
            url("https://example.com/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz")
        );
    }

    #[test]
    fn different_url() {
        let meta = PkgMeta {
            pkg_url:
            "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ archive-format }"
                .into(),
            ..Default::default()
        };

        let data = Data {
            name: "radio-sx128x".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            version: "0.14.1-alpha.5".to_string(),
            repo: Some("https://github.com/rust-iot/rust-radio-sx128x".to_string()),
            meta,
        };

        let ctx = Context::from_data(&data, "tgz");
        assert_eq!(
            ctx.render_url(&data.meta.pkg_url).unwrap(),
            url("https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz")
        );
    }

    #[test]
    fn deprecated_format() {
        let meta = PkgMeta {
            pkg_url: "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ format }".into(),
            ..Default::default()
        };

        let data = Data {
            name: "radio-sx128x".to_string(),
            target: "x86_64-unknown-linux-gnu".to_string(),
            version: "0.14.1-alpha.5".to_string(),
            repo: Some("https://github.com/rust-iot/rust-radio-sx128x".to_string()),
            meta,
        };

        let ctx = Context::from_data(&data, "tgz");
        assert_eq!(
            ctx.render_url(&data.meta.pkg_url).unwrap(),
            url("https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz")
        );
    }

    #[test]
    fn different_ext() {
        let meta = PkgMeta {
            pkg_url:
                "{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }.tar.xz"
                    .into(),
            pkg_fmt: PkgFmt::Txz,
            ..Default::default()
        };

        let data = Data {
            name: "cargo-watch".to_string(),
            target: "aarch64-apple-darwin".to_string(),
            version: "9.0.0".to_string(),
            repo: Some("https://github.com/watchexec/cargo-watch".to_string()),
            meta,
        };

        let ctx = Context::from_data(&data, "txz");
        assert_eq!(
            ctx.render_url(&data.meta.pkg_url).unwrap(),
            url("https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-apple-darwin.tar.xz")
        );
    }

    #[test]
    fn no_archive() {
        let meta = PkgMeta {
            pkg_url: "{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }{ binary-ext }".into(),
            pkg_fmt: PkgFmt::Bin,
            ..Default::default()
        };

        let data = Data {
            name: "cargo-watch".to_string(),
            target: "aarch64-pc-windows-msvc".to_string(),
            version: "9.0.0".to_string(),
            repo: Some("https://github.com/watchexec/cargo-watch".to_string()),
            meta,
        };

        let ctx = Context::from_data(&data, "bin");
        assert_eq!(
            ctx.render_url(&data.meta.pkg_url).unwrap(),
            url("https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-pc-windows-msvc.exe")
        );
    }
}
