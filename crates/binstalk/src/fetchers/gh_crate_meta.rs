use std::{future::Future, iter, ops::Deref, path::Path, sync::Arc};

use compact_str::{CompactString, ToCompactString};
use either::Either;
use futures_util::stream::{FuturesUnordered, StreamExt};
use once_cell::sync::OnceCell;
use serde::Serialize;
use strum::IntoEnumIterator;
use tinytemplate::TinyTemplate;
use tracing::{debug, warn};
use url::Url;

use crate::{
    errors::BinstallError,
    helpers::{
        download::Download,
        remote::{Client, Method},
        tasks::AutoAbortJoinHandle,
    },
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

use super::{Data, TargetData};

pub(crate) mod hosting;
use hosting::RepositoryHost;

pub struct GhCrateMeta {
    client: Client,
    data: Arc<Data>,
    target_data: Arc<TargetData>,
    resolution: OnceCell<(Url, PkgFmt)>,
}

type FindTaskRes = Result<Option<(Url, PkgFmt)>, BinstallError>;

impl GhCrateMeta {
    fn launch_baseline_find_tasks<'a>(
        &'a self,
        pkg_fmt: PkgFmt,
        pkg_url: &'a str,
        repo: Option<&'a str>,
    ) -> impl Iterator<Item = impl Future<Output = FindTaskRes> + 'static> + 'a {
        // build up list of potential URLs
        let urls = pkg_fmt.extensions().iter().filter_map(move |ext| {
            let ctx = Context::from_data_with_repo(&self.data, &self.target_data.target, ext, repo);
            match ctx.render_url(pkg_url) {
                Ok(url) => Some(url),
                Err(err) => {
                    warn!("Failed to render url for {ctx:#?}: {err:#?}");
                    None
                }
            }
        });

        // go check all potential URLs at once
        urls.map(move |url| {
            let client = self.client.clone();

            async move {
                debug!("Checking for package at: '{url}'");

                Ok((client.remote_exists(url.clone(), Method::HEAD).await?
                    || client.remote_exists(url.clone(), Method::GET).await?)
                    .then_some((url, pkg_fmt)))
            }
        })
    }
}

#[async_trait::async_trait]
impl super::Fetcher for GhCrateMeta {
    fn new(
        client: Client,
        data: Arc<Data>,
        target_data: Arc<TargetData>,
    ) -> Arc<dyn super::Fetcher> {
        Arc::new(Self {
            client,
            data,
            target_data,
            resolution: OnceCell::new(),
        })
    }

    fn find(self: Arc<Self>) -> AutoAbortJoinHandle<Result<bool, BinstallError>> {
        AutoAbortJoinHandle::spawn(async move {
            let repo = if let Some(repo) = self.data.repo.as_deref() {
                Some(
                    self.client
                        .get_redirected_final_url(Url::parse(repo)?)
                        .await?,
                )
            } else {
                None
            };

            let pkg_urls = if let Some(pkg_url) = self.target_data.meta.pkg_url.as_deref() {
                Either::Left(pkg_url)
            } else if let Some(repo) = repo.as_ref() {
                if let Some(pkg_urls) =
                    RepositoryHost::guess_git_hosting_services(repo)?.get_default_pkg_url_template()
                {
                    Either::Right(pkg_urls)
                } else {
                    warn!(
                    concat!(
                        "Unknown repository {}, cargo-binstall cannot provide default pkg_url for it.\n",
                        "Please ask the upstream to provide it for target {}."
                    ),
                    repo, self.target_data.target
                );

                    return Ok(false);
                }
            } else {
                warn!(
                concat!(
                    "Package does not specify repository, cargo-binstall cannot provide default pkg_url for it.\n",
                    "Please ask the upstream to provide it for target {}."
                ),
                self.target_data.target
            );

                return Ok(false);
            };

            // Convert Option<Url> to Option<String> to reduce size of future.
            let repo = repo.map(String::from);
            let repo = repo.as_deref().map(|u| u.trim_end_matches('/'));

            // Use reference to self to fix error of closure
            // launch_baseline_find_tasks which moves `this`
            let this = &self;

            let launch_baseline_find_tasks = |pkg_fmt| {
                match &pkg_urls {
                    Either::Left(pkg_url) => Either::Left(iter::once(*pkg_url)),
                    Either::Right(pkg_urls) => Either::Right(pkg_urls.iter().map(Deref::deref)),
                }
                .flat_map(move |pkg_url| this.launch_baseline_find_tasks(pkg_fmt, pkg_url, repo))
            };

            let mut handles: FuturesUnordered<_> =
                if let Some(pkg_fmt) = self.target_data.meta.pkg_fmt {
                    launch_baseline_find_tasks(pkg_fmt).collect()
                } else {
                    PkgFmt::iter()
                        .flat_map(launch_baseline_find_tasks)
                        .collect()
                };

            while let Some(res) = handles.next().await {
                if let Some((url, pkg_fmt)) = res? {
                    debug!("Winning URL is {url}, with pkg_fmt {pkg_fmt}");
                    self.resolution.set((url, pkg_fmt)).unwrap(); // find() is called first
                    return Ok(true);
                }
            }

            Ok(false)
        })
    }

    async fn fetch_and_extract(&self, dst: &Path) -> Result<(), BinstallError> {
        let (url, pkg_fmt) = self.resolution.get().unwrap(); // find() is called first
        debug!("Downloading package from: '{url}' dst:{dst:?} fmt:{pkg_fmt:?}");
        Ok(Download::new(self.client.clone(), url.clone())
            .and_extract(*pkg_fmt, dst)
            .await?)
    }

    fn pkg_fmt(&self) -> PkgFmt {
        self.resolution.get().unwrap().1
    }

    fn target_meta(&self) -> PkgMeta {
        let mut meta = self.target_data.meta.clone();
        meta.pkg_fmt = Some(self.pkg_fmt());
        meta
    }

    fn source_name(&self) -> CompactString {
        self.resolution
            .get()
            .map(|(url, _pkg_fmt)| {
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

    fn fetcher_name(&self) -> &'static str {
        "GhCrateMeta"
    }

    fn is_third_party(&self) -> bool {
        false
    }

    fn target(&self) -> &str {
        &self.target_data.target
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

    #[serde(rename = "archive-suffix")]
    pub archive_suffix: &'c str,

    /// Filename extension on the binary, i.e. .exe on Windows, nothing otherwise
    #[serde(rename = "binary-ext")]
    pub binary_ext: &'c str,
}

impl<'c> Context<'c> {
    pub(self) fn from_data_with_repo(
        data: &'c Data,
        target: &'c str,
        archive_suffix: &'c str,
        repo: Option<&'c str>,
    ) -> Self {
        let archive_format = if archive_suffix.is_empty() {
            // Empty archive_suffix means PkgFmt::Bin
            "bin"
        } else {
            debug_assert!(archive_suffix.starts_with('.'), "{archive_suffix}");

            &archive_suffix[1..]
        };

        Self {
            name: &data.name,
            repo,
            target,
            version: &data.version,
            format: archive_format,
            archive_format,
            archive_suffix,
            binary_ext: if target.contains("windows") {
                ".exe"
            } else {
                ""
            },
        }
    }

    #[cfg(test)]
    pub(self) fn from_data(data: &'c Data, target: &'c str, archive_format: &'c str) -> Self {
        Self::from_data_with_repo(data, target, archive_format, data.repo.as_deref())
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
    use compact_str::ToCompactString;
    use url::Url;

    const DEFAULT_PKG_URL: &str = "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ archive-format }";

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn defaults() {
        let data = Data {
            name: "cargo-binstall".to_compact_string(),
            version: "1.2.3".to_compact_string(),
            repo: Some("https://github.com/ryankurte/cargo-binstall".to_string()),
        };

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        assert_eq!(
            ctx.render_url(DEFAULT_PKG_URL).unwrap(),
            url("https://github.com/ryankurte/cargo-binstall/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz")
        );
    }

    #[test]
    #[should_panic]
    fn no_repo() {
        let meta = PkgMeta::default();
        let data = Data {
            name: "cargo-binstall".to_compact_string(),
            version: "1.2.3".to_compact_string(),
            repo: None,
        };

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        ctx.render_url(meta.pkg_url.as_deref().unwrap()).unwrap();
    }

    #[test]
    fn no_repo_but_full_url() {
        let meta = PkgMeta {
            pkg_url: Some(format!("https://example.com{DEFAULT_PKG_URL}")),
            ..Default::default()
        };

        let data = Data {
            name: "cargo-binstall".to_compact_string(),
            version: "1.2.3".to_compact_string(),
            repo: None,
        };

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        assert_eq!(
            ctx.render_url(meta.pkg_url.as_deref().unwrap()).unwrap(),
            url("https://example.com/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz")
        );
    }

    #[test]
    fn different_url() {
        let meta = PkgMeta {
            pkg_url: Some(
            "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ archive-format }"
                .to_string()),
            ..Default::default()
        };

        let data = Data {
            name: "radio-sx128x".to_compact_string(),
            version: "0.14.1-alpha.5".to_compact_string(),
            repo: Some("https://github.com/rust-iot/rust-radio-sx128x".to_string()),
        };

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        assert_eq!(
            ctx.render_url(meta.pkg_url.as_deref().unwrap()).unwrap(),
            url("https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz")
        );
    }

    #[test]
    fn deprecated_format() {
        let meta = PkgMeta {
            pkg_url: Some("{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ format }".to_string()),
            ..Default::default()
        };

        let data = Data {
            name: "radio-sx128x".to_compact_string(),
            version: "0.14.1-alpha.5".to_compact_string(),
            repo: Some("https://github.com/rust-iot/rust-radio-sx128x".to_string()),
        };

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        assert_eq!(
            ctx.render_url(meta.pkg_url.as_deref().unwrap()).unwrap(),
            url("https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz")
        );
    }

    #[test]
    fn different_ext() {
        let meta = PkgMeta {
            pkg_url: Some(
                "{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }.tar.xz"
                    .to_string(),
            ),
            pkg_fmt: Some(PkgFmt::Txz),
            ..Default::default()
        };

        let data = Data {
            name: "cargo-watch".to_compact_string(),
            version: "9.0.0".to_compact_string(),
            repo: Some("https://github.com/watchexec/cargo-watch".to_string()),
        };

        let ctx = Context::from_data(&data, "aarch64-apple-darwin", ".txz");
        assert_eq!(
            ctx.render_url(meta.pkg_url.as_deref().unwrap()).unwrap(),
            url("https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-apple-darwin.tar.xz")
        );
    }

    #[test]
    fn no_archive() {
        let meta = PkgMeta {
            pkg_url: Some("{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }{ binary-ext }".to_string()),
            pkg_fmt: Some(PkgFmt::Bin),
            ..Default::default()
        };

        let data = Data {
            name: "cargo-watch".to_compact_string(),
            version: "9.0.0".to_compact_string(),
            repo: Some("https://github.com/watchexec/cargo-watch".to_string()),
        };

        let ctx = Context::from_data(&data, "aarch64-pc-windows-msvc", ".bin");
        assert_eq!(
            ctx.render_url(meta.pkg_url.as_deref().unwrap()).unwrap(),
            url("https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-pc-windows-msvc.exe")
        );
    }
}
