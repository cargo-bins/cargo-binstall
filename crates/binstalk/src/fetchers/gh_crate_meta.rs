use std::{borrow::Cow, iter, path::Path, sync::Arc};

use compact_str::{CompactString, ToCompactString};
use either::Either;
use leon::Template;
use once_cell::sync::OnceCell;
use strum::IntoEnumIterator;
use tracing::{debug, warn};
use url::Url;

use crate::{
    errors::{BinstallError, InvalidPkgFmtError},
    helpers::{
        download::{Download, ExtractedFiles},
        futures_resolver::FuturesResolver,
        gh_api_client::GhApiClient,
        remote::{does_url_exist, Client},
        tasks::AutoAbortJoinHandle,
    },
    manifests::cargo_toml_binstall::{PkgFmt, PkgMeta},
};

use super::{Data, RepoInfo, TargetData};

pub(crate) mod hosting;

pub struct GhCrateMeta {
    client: Client,
    gh_api_client: GhApiClient,
    data: Arc<Data>,
    target_data: Arc<TargetData>,
    resolution: OnceCell<(Url, PkgFmt)>,
}

impl GhCrateMeta {
    fn launch_baseline_find_tasks(
        &self,
        futures_resolver: &FuturesResolver<(Url, PkgFmt), BinstallError>,
        pkg_fmt: PkgFmt,
        pkg_url: &Template<'_>,
        repo: Option<&str>,
        subcrate: Option<&str>,
    ) {
        let render_url = |ext| {
            let ctx = Context::from_data_with_repo(
                &self.data,
                &self.target_data.target,
                ext,
                repo,
                subcrate,
            );
            match ctx.render_url_with_compiled_tt(pkg_url) {
                Ok(url) => Some(url),
                Err(err) => {
                    warn!("Failed to render url for {ctx:#?}: {err}");
                    None
                }
            }
        };

        let is_windows = self.target_data.target.contains("windows");

        let urls = if pkg_url.has_any_of_keys(&["format", "archive-format", "archive-suffix"]) {
            // build up list of potential URLs
            Either::Left(
                pkg_fmt
                    .extensions(is_windows)
                    .iter()
                    .filter_map(|ext| render_url(Some(ext))),
            )
        } else {
            Either::Right(render_url(None).into_iter())
        };

        // go check all potential URLs at once
        futures_resolver.extend(urls.map(move |url| {
            let client = self.client.clone();
            let gh_api_client = self.gh_api_client.clone();

            async move {
                Ok(does_url_exist(client, gh_api_client, &url)
                    .await?
                    .then_some((url, pkg_fmt)))
            }
        }));
    }
}

#[async_trait::async_trait]
impl super::Fetcher for GhCrateMeta {
    fn new(
        client: Client,
        gh_api_client: GhApiClient,
        data: Arc<Data>,
        target_data: Arc<TargetData>,
    ) -> Arc<dyn super::Fetcher> {
        Arc::new(Self {
            client,
            gh_api_client,
            data,
            target_data,
            resolution: OnceCell::new(),
        })
    }

    fn find(self: Arc<Self>) -> AutoAbortJoinHandle<Result<bool, BinstallError>> {
        AutoAbortJoinHandle::spawn(async move {
            let info = self.data.get_repo_info(&self.client).await?.as_ref();

            let repo = info.map(|info| &info.repo);
            let subcrate = info.and_then(|info| info.subcrate.as_deref());

            let mut pkg_fmt = self.target_data.meta.pkg_fmt;

            let pkg_urls = if let Some(pkg_url) = self.target_data.meta.pkg_url.as_deref() {
                let template = Template::parse(pkg_url)?;

                if pkg_fmt.is_none()
                    && !template.has_any_of_keys(&["format", "archive-format", "archive-suffix"])
                {
                    // The crate does not specify the pkg-fmt, yet its pkg-url
                    // template doesn't contains format, archive-format or
                    // archive-suffix which is required for automatically
                    // deducing the pkg-fmt.
                    //
                    // We will attempt to guess the pkg-fmt there, but this is
                    // just a best-effort
                    pkg_fmt = PkgFmt::guess_pkg_format(pkg_url);

                    let crate_name = &self.data.name;
                    let version = &self.data.version;
                    let target = &self.target_data.target;

                    if pkg_fmt.is_none() {
                        return Err(InvalidPkgFmtError {
                            crate_name: crate_name.clone(),
                            version: version.clone(),
                            target: target.clone(),
                            pkg_url: pkg_url.to_string(),
                            reason: "pkg-fmt is not specified, yet pkg-url does not contain format, archive-format or archive-suffix which is required for automatically deducing pkg-fmt",
                        }
                        .into());
                    }

                    warn!(
                        "Crate {crate_name}@{version} on target {target} does not specify pkg-fmt \
                        but its pkg-url also does not contain key format, archive-format or \
                        archive-suffix.\nbinstall was able to guess that from pkg-url, but \
                        just note that it could be wrong:\npkg-fmt=\"{pkg_fmt}\", pkg-url=\"{pkg_url}\"",
                        pkg_fmt = pkg_fmt.unwrap(),
                    );
                }

                Either::Left(iter::once(template))
            } else if let Some(RepoInfo {
                repo,
                repository_host,
                ..
            }) = info
            {
                if let Some(pkg_urls) = repository_host.get_default_pkg_url_template() {
                    let has_subcrate = subcrate.is_some();

                    Either::Right(
                        pkg_urls
                            .map(Template::cast)
                            // If subcrate is Some, then all templates will be included.
                            // Otherwise, only templates without key "subcrate" will be
                            // included.
                            .filter(move |template| has_subcrate || !template.has_key("subcrate")),
                    )
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
            let repo = repo.map(|u| u.as_str().trim_end_matches('/'));

            // Use reference to self to fix error of closure
            // launch_baseline_find_tasks which moves `this`
            let this = &self;

            let pkg_fmts = if let Some(pkg_fmt) = pkg_fmt {
                Either::Left(iter::once(pkg_fmt))
            } else {
                Either::Right(PkgFmt::iter())
            };

            let resolver = FuturesResolver::default();

            // Iterate over pkg_urls first to avoid String::clone.
            for pkg_url in pkg_urls {
                //             Clone iter pkg_fmts to ensure all pkg_fmts is
                //             iterated over for each pkg_url, which is
                //             basically cartesian product.
                //             |
                for pkg_fmt in pkg_fmts.clone() {
                    this.launch_baseline_find_tasks(&resolver, pkg_fmt, &pkg_url, repo, subcrate);
                }
            }

            if let Some((url, pkg_fmt)) = resolver.resolve().await? {
                debug!("Winning URL is {url}, with pkg_fmt {pkg_fmt}");
                self.resolution.set((url, pkg_fmt)).unwrap(); // find() is called first
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, BinstallError> {
        let (url, pkg_fmt) = self.resolution.get().unwrap(); // find() is called first
        debug!(
            "Downloading package from: '{url}' dst:{} fmt:{pkg_fmt:?}",
            dst.display()
        );
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
#[derive(Clone, Debug)]
struct Context<'c> {
    pub name: &'c str,
    pub repo: Option<&'c str>,
    pub target: &'c str,
    pub version: &'c str,

    /// Archive format e.g. tar.gz, zip
    pub archive_format: Option<&'c str>,

    pub archive_suffix: Option<&'c str>,

    /// Filename extension on the binary, i.e. .exe on Windows, nothing otherwise
    pub binary_ext: &'c str,

    /// Workspace of the crate inside the repository.
    pub subcrate: Option<&'c str>,
}

impl leon::Values for Context<'_> {
    fn get_value<'s>(&'s self, key: &str) -> Option<Cow<'s, str>> {
        match key {
            "name" => Some(Cow::Borrowed(self.name)),
            "repo" => self.repo.map(Cow::Borrowed),
            "target" => Some(Cow::Borrowed(self.target)),
            "version" => Some(Cow::Borrowed(self.version)),

            "archive-format" => self.archive_format.map(Cow::Borrowed),

            // Soft-deprecated alias for archive-format
            "format" => self.archive_format.map(Cow::Borrowed),

            "archive-suffix" => self.archive_suffix.map(Cow::Borrowed),

            "binary-ext" => Some(Cow::Borrowed(self.binary_ext)),

            "subcrate" => self.subcrate.map(Cow::Borrowed),

            _ => None,
        }
    }
}

impl<'c> Context<'c> {
    pub(self) fn from_data_with_repo(
        data: &'c Data,
        target: &'c str,
        archive_suffix: Option<&'c str>,
        repo: Option<&'c str>,
        subcrate: Option<&'c str>,
    ) -> Self {
        let archive_format = archive_suffix.map(|archive_suffix| {
            if archive_suffix.is_empty() {
                // Empty archive_suffix means PkgFmt::Bin
                "bin"
            } else {
                debug_assert!(archive_suffix.starts_with('.'), "{archive_suffix}");

                &archive_suffix[1..]
            }
        });

        Self {
            name: &data.name,
            repo,
            target,
            version: &data.version,
            archive_format,
            archive_suffix,
            binary_ext: if target.contains("windows") {
                ".exe"
            } else {
                ""
            },
            subcrate,
        }
    }

    #[cfg(test)]
    pub(self) fn from_data(data: &'c Data, target: &'c str, archive_format: &'c str) -> Self {
        Self::from_data_with_repo(
            data,
            target,
            Some(archive_format),
            data.repo.as_deref(),
            None,
        )
    }

    /// * `tt` - must have added a template named "pkg_url".
    pub(self) fn render_url_with_compiled_tt(
        &self,
        tt: &Template<'_>,
    ) -> Result<Url, BinstallError> {
        debug!("Render {tt:#?} using context: {self:?}");

        Ok(Url::parse(&tt.render(self)?)?)
    }

    #[cfg(test)]
    pub(self) fn render_url(&self, template: &str) -> Result<Url, BinstallError> {
        debug!("Render {template} using context in render_url: {self:?}");

        let tt = Template::parse(template)?;
        self.render_url_with_compiled_tt(&tt)
    }
}

#[cfg(test)]
mod test {
    use crate::manifests::cargo_toml_binstall::PkgMeta;

    use super::{super::Data, Context};
    use compact_str::ToCompactString;
    use url::Url;

    const DEFAULT_PKG_URL: &str = "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ archive-format }";

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn defaults() {
        let data = Data::new(
            "cargo-binstall".to_compact_string(),
            "1.2.3".to_compact_string(),
            Some("https://github.com/ryankurte/cargo-binstall".to_string()),
        );

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
        let data = Data::new(
            "cargo-binstall".to_compact_string(),
            "1.2.3".to_compact_string(),
            None,
        );

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        ctx.render_url(meta.pkg_url.as_deref().unwrap()).unwrap();
    }

    #[test]
    fn no_repo_but_full_url() {
        let pkg_url = &format!("https://example.com{}", &DEFAULT_PKG_URL[8..]);

        let data = Data::new(
            "cargo-binstall".to_compact_string(),
            "1.2.3".to_compact_string(),
            None,
        );

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        assert_eq!(
            ctx.render_url(pkg_url).unwrap(),
            url("https://example.com/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz")
        );
    }

    #[test]
    fn different_url() {
        let pkg_url =
            "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ archive-format }";

        let data = Data::new(
            "radio-sx128x".to_compact_string(),
            "0.14.1-alpha.5".to_compact_string(),
            Some("https://github.com/rust-iot/rust-radio-sx128x".to_string()),
        );

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        assert_eq!(
            ctx.render_url(pkg_url).unwrap(),
            url("https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz")
        );
    }

    #[test]
    fn deprecated_format() {
        let pkg_url = "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ format }";

        let data = Data::new(
            "radio-sx128x".to_compact_string(),
            "0.14.1-alpha.5".to_compact_string(),
            Some("https://github.com/rust-iot/rust-radio-sx128x".to_string()),
        );

        let ctx = Context::from_data(&data, "x86_64-unknown-linux-gnu", ".tgz");
        assert_eq!(
            ctx.render_url(pkg_url).unwrap(),
            url("https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz")
        );
    }

    #[test]
    fn different_ext() {
        let pkg_url =
            "{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }.tar.xz";

        let data = Data::new(
            "cargo-watch".to_compact_string(),
            "9.0.0".to_compact_string(),
            Some("https://github.com/watchexec/cargo-watch".to_string()),
        );

        let ctx = Context::from_data(&data, "aarch64-apple-darwin", ".txz");
        assert_eq!(
            ctx.render_url(pkg_url).unwrap(),
            url("https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-apple-darwin.tar.xz")
        );
    }

    #[test]
    fn no_archive() {
        let pkg_url =  "{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }{ binary-ext }"
        ;

        let data = Data::new(
            "cargo-watch".to_compact_string(),
            "9.0.0".to_compact_string(),
            Some("https://github.com/watchexec/cargo-watch".to_string()),
        );

        let ctx = Context::from_data(&data, "aarch64-pc-windows-msvc", ".bin");
        assert_eq!(
            ctx.render_url(pkg_url).unwrap(),
            url("https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-pc-windows-msvc.exe")
        );
    }
}
