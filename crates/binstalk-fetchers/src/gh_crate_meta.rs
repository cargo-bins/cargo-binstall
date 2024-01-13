use std::{borrow::Cow, fmt, iter, path::Path, sync::Arc};

use compact_str::{CompactString, ToCompactString};
use either::Either;
use leon::Template;
use once_cell::sync::OnceCell;
use strum::IntoEnumIterator;
use tracing::{debug, info, trace, warn};
use url::Url;

use crate::{
    common::*, futures_resolver::FuturesResolver, Data, FetchError, InvalidPkgFmtError, RepoInfo,
    SignaturePolicy, SignatureVerifier, TargetDataErased,
};

pub(crate) mod hosting;

pub struct GhCrateMeta {
    client: Client,
    gh_api_client: GhApiClient,
    data: Arc<Data>,
    target_data: Arc<TargetDataErased>,
    signature_policy: SignaturePolicy,
    resolution: OnceCell<Resolved>,
}

#[derive(Debug)]
struct Resolved {
    url: Url,
    pkg_fmt: PkgFmt,
    archive_suffix: Option<String>,
    repo: Option<String>,
    subcrate: Option<String>,
}

impl GhCrateMeta {
    fn launch_baseline_find_tasks(
        &self,
        futures_resolver: &FuturesResolver<Resolved, FetchError>,
        pkg_fmt: PkgFmt,
        pkg_url: &Template<'_>,
        repo: Option<&str>,
        subcrate: Option<&str>,
    ) {
        let render_url = |ext| {
            let ctx = Context::from_data_with_repo(
                &self.data,
                &self.target_data.target,
                &self.target_data.target_related_info,
                ext,
                repo,
                subcrate,
            );
            match ctx.render_url_with(pkg_url) {
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
                    .filter_map(|ext| render_url(Some(ext)).map(|url| (url, Some(ext)))),
            )
        } else {
            Either::Right(render_url(None).map(|url| (url, None)).into_iter())
        };

        // go check all potential URLs at once
        futures_resolver.extend(urls.map(move |(url, ext)| {
            let client = self.client.clone();
            let gh_api_client = self.gh_api_client.clone();

            let repo = repo.map(ToString::to_string);
            let subcrate = subcrate.map(ToString::to_string);
            let archive_suffix = ext.map(ToString::to_string);
            async move {
                Ok(does_url_exist(client, gh_api_client, &url)
                    .await?
                    .then_some(Resolved {
                        url,
                        pkg_fmt,
                        repo,
                        subcrate,
                        archive_suffix,
                    }))
            }
        }));
    }
}

#[async_trait::async_trait]
impl super::Fetcher for GhCrateMeta {
    fn new(
        client: Client,
        gh_api_client: GhApiClient,
        _cacher: HTTPCacher,
        data: Arc<Data>,
        target_data: Arc<TargetDataErased>,
        signature_policy: SignaturePolicy,
    ) -> Arc<dyn super::Fetcher> {
        Arc::new(Self {
            client,
            gh_api_client,
            data,
            target_data,
            signature_policy,
            resolution: OnceCell::new(),
        })
    }

    fn find(self: Arc<Self>) -> JoinHandle<Result<bool, FetchError>> {
        tokio::spawn(async move {
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
                            target: target.into(),
                            pkg_url: pkg_url.into(),
                            reason:
                                &"pkg-fmt is not specified, yet pkg-url does not contain format, \
                                archive-format or archive-suffix which is required for automatically \
                                deducing pkg-fmt",
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

            if let Some(resolved) = resolver.resolve().await? {
                debug!(?resolved, "Winning URL found!");
                self.resolution.set(resolved).unwrap(); // find() is called first
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, FetchError> {
        let resolved = self.resolution.get().unwrap(); // find() is called first
        trace!(?resolved, "preparing to fetch");

        let verifier = match (self.signature_policy, &self.target_data.meta.signing) {
            (SignaturePolicy::Ignore, _) | (SignaturePolicy::IfPresent, None) => {
                SignatureVerifier::Noop
            }
            (SignaturePolicy::Require, None) => {
                return Err(FetchError::MissingSignature);
            }
            (_, Some(config)) => {
                let template = match config.file.as_deref() {
                    Some(file) => Template::parse(file)?,
                    None => leon_macros::template!("{ url }.sig"),
                };
                trace!(?template, "parsed signature file template");

                let sign_url = Context::from_data_with_repo(
                    &self.data,
                    &self.target_data.target,
                    &self.target_data.target_related_info,
                    resolved.archive_suffix.as_deref(),
                    resolved.repo.as_deref(),
                    resolved.subcrate.as_deref(),
                )
                .with_url(&resolved.url)
                .render_url_with(&template)?;

                debug!(?sign_url, "Downloading signature");
                let signature = Download::new(self.client.clone(), sign_url)
                    .into_bytes()
                    .await?;
                trace!(?signature, "got signature contents");

                SignatureVerifier::new(config, &signature)?
            }
        };

        debug!(
            url=%resolved.url,
            dst=%dst.display(),
            fmt=?resolved.pkg_fmt,
            "Downloading package",
        );
        let mut data_verifier = verifier.data_verifier()?;
        let files = Download::new_with_data_verifier(
            self.client.clone(),
            resolved.url.clone(),
            data_verifier.as_mut(),
        )
        .and_extract(resolved.pkg_fmt, dst)
        .await?;
        trace!("validating signature (if any)");
        if data_verifier.validate() {
            if let Some(info) = verifier.info() {
                info!(
                    "Verified signature for package '{}': {info}",
                    self.data.name
                );
            }
            Ok(files)
        } else {
            Err(FetchError::InvalidSignature)
        }
    }

    fn pkg_fmt(&self) -> PkgFmt {
        self.resolution.get().unwrap().pkg_fmt
    }

    fn target_meta(&self) -> PkgMeta {
        let mut meta = self.target_data.meta.clone();
        meta.pkg_fmt = Some(self.pkg_fmt());
        meta
    }

    fn source_name(&self) -> CompactString {
        self.resolution
            .get()
            .map(|resolved| {
                if let Some(domain) = resolved.url.domain() {
                    domain.to_compact_string()
                } else if let Some(host) = resolved.url.host_str() {
                    host.to_compact_string()
                } else {
                    resolved.url.to_compact_string()
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

    fn target_data(&self) -> &Arc<TargetDataErased> {
        &self.target_data
    }
}

/// Template for constructing download paths
#[derive(Clone)]
struct Context<'c> {
    name: &'c str,
    repo: Option<&'c str>,
    target: &'c str,
    version: &'c str,

    /// Archive format e.g. tar.gz, zip
    archive_format: Option<&'c str>,

    archive_suffix: Option<&'c str>,

    /// Filename extension on the binary, i.e. .exe on Windows, nothing otherwise
    binary_ext: &'c str,

    /// Workspace of the crate inside the repository.
    subcrate: Option<&'c str>,

    /// Url of the file being downloaded (only for signing.file)
    url: Option<&'c Url>,

    target_related_info: &'c dyn leon::Values,
}

impl fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("name", &self.name)
            .field("repo", &self.repo)
            .field("target", &self.target)
            .field("version", &self.version)
            .field("archive_format", &self.archive_format)
            .field("binary_ext", &self.binary_ext)
            .field("subcrate", &self.subcrate)
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
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

            "url" => self.url.map(|url| Cow::Borrowed(url.as_str())),

            key => self.target_related_info.get_value(key),
        }
    }
}

impl<'c> Context<'c> {
    fn from_data_with_repo(
        data: &'c Data,
        target: &'c str,
        target_related_info: &'c dyn leon::Values,
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
            url: None,

            target_related_info,
        }
    }

    fn with_url(&mut self, url: &'c Url) -> &mut Self {
        self.url = Some(url);
        self
    }

    fn render_url_with(&self, template: &Template<'_>) -> Result<Url, FetchError> {
        debug!(?template, context=?self, "render url template");
        Ok(Url::parse(&template.render(self)?)?)
    }

    #[cfg(test)]
    fn render_url(&self, template: &str) -> Result<Url, FetchError> {
        self.render_url_with(&Template::parse(template)?)
    }
}

#[cfg(test)]
mod test {
    use super::{super::Data, Context};
    use compact_str::ToCompactString;
    use url::Url;

    const DEFAULT_PKG_URL: &str = "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ archive-format }";

    fn assert_context_rendering(
        data: &Data,
        target: &str,
        archive_format: &str,
        template: &str,
        expected_url: &str,
    ) {
        // The template provided doesn't need this, so just returning None
        // is OK.
        let target_info = leon::vals(|_| None);

        let ctx = Context::from_data_with_repo(
            data,
            target,
            &target_info,
            Some(archive_format),
            data.repo.as_deref(),
            None,
        );

        let expected_url = Url::parse(expected_url).unwrap();
        assert_eq!(ctx.render_url(template).unwrap(), expected_url);
    }

    #[test]
    fn defaults() {
        assert_context_rendering(
            &Data::new(
                "cargo-binstall".to_compact_string(),
                "1.2.3".to_compact_string(),
                Some("https://github.com/ryankurte/cargo-binstall".to_string()),
            ),
            "x86_64-unknown-linux-gnu",
            ".tgz",
            DEFAULT_PKG_URL,
            "https://github.com/ryankurte/cargo-binstall/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz"
        );
    }

    #[test]
    fn no_repo_but_full_url() {
        assert_context_rendering(
            &Data::new(
                "cargo-binstall".to_compact_string(),
                "1.2.3".to_compact_string(),
                None,
            ),
            "x86_64-unknown-linux-gnu",
            ".tgz",
            &format!("https://example.com{}", &DEFAULT_PKG_URL[8..]),
            "https://example.com/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz"
        );
    }

    #[test]
    fn different_url() {
        assert_context_rendering(
            &Data::new(
                "radio-sx128x".to_compact_string(),
                "0.14.1-alpha.5".to_compact_string(),
                Some("https://github.com/rust-iot/rust-radio-sx128x".to_string()),
            ),
            "x86_64-unknown-linux-gnu",
            ".tgz",
            "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ archive-format }",
            "https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz"
        );
    }

    #[test]
    fn deprecated_format() {
        assert_context_rendering(
            &Data::new(
                "radio-sx128x".to_compact_string(),
                "0.14.1-alpha.5".to_compact_string(),
                Some("https://github.com/rust-iot/rust-radio-sx128x".to_string()),
            ),
            "x86_64-unknown-linux-gnu",
            ".tgz",
            "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ format }",
            "https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz"
        );
    }

    #[test]
    fn different_ext() {
        assert_context_rendering(
            &Data::new(
                "cargo-watch".to_compact_string(),
                "9.0.0".to_compact_string(),
                Some("https://github.com/watchexec/cargo-watch".to_string()),
            ),
            "aarch64-apple-darwin",
            ".txz",
            "{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }.tar.xz",
            "https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-apple-darwin.tar.xz"
        );
    }

    #[test]
    fn no_archive() {
        assert_context_rendering(
            &Data::new(
                "cargo-watch".to_compact_string(),
                "9.0.0".to_compact_string(),
                Some("https://github.com/watchexec/cargo-watch".to_string()),
            ),
            "aarch64-pc-windows-msvc",
            ".bin",
            "{ repo }/releases/download/v{ version }/{ name }-v{ version }-{ target }{ binary-ext }",
            "https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-pc-windows-msvc.exe"
        );
    }
}
