use std::{
    borrow::Cow,
    fmt, iter,
    path::Path,
    sync::{Arc, OnceLock},
};

use binstalk_downloader::download::DataVerifier;
use binstalk_git_repo_api::gh_api_client::GhReleaseArtifactUrl;
use binstalk_types::{
    cargo_toml_binstall::Strategy,
    crate_info::{PkgUrlHint, RenderedWith},
};
use compact_str::{CompactString, ToCompactString};
use either::Either;
use itertools::Itertools;
use leon::Template;
use strum::IntoEnumIterator;
use tracing::{debug, info, trace, warn};
use url::Url;

use crate::{
    Data, FetchError, InvalidPkgFmtError, RepoInfo, SignaturePolicy, SignatureVerifier,
    TargetDataErased, common::*,
};

pub const FETCHER_GH_CRATE_META: &str = "GhCrateMeta";

mod checksum;
pub(crate) mod hosting;
mod probe;

use probe::{Candidate, Probe};

pub struct GhCrateMeta {
    client: Client,
    gh_api_client: GhApiClient,
    data: Arc<Data>,
    target_data: Arc<TargetDataErased>,
    signature_policy: SignaturePolicy,
    resolution: OnceLock<Resolved>,
}

#[derive(Debug)]
struct Resolved {
    url: Url,
    pkg_fmt: PkgFmt,
    archive_suffix: Option<String>,
    repo: Option<CompactString>,
    subcrate: Option<CompactString>,
    gh_release_artifact_url: Option<GhReleaseArtifactUrl>,
    is_repo_private: bool,
    /// The alias from [`target_arch_aliases`] that `{ target-arch }` was
    /// rendered as in `url`, if not the architecture's own name.
    target_arch: Option<&'static str>,
    /// The `pkg-url` that `url` was rendered from, see
    /// [`PkgUrlHint::pkg_url`].
    pkg_url: Arc<str>,
}

/// Feeds the package to the signature verifier and to the checksum
/// verifier, if any.
struct Verifiers<'a, 'b, 'c> {
    signature: &'a mut (dyn DataVerifier + 'b),
    checksum: Option<&'a mut checksum::ChecksumVerifier<'c>>,
}

impl DataVerifier for Verifiers<'_, '_, '_> {
    fn update(&mut self, data: &bytes::Bytes) {
        self.signature.update(data);
        if let Some(checksum) = &mut self.checksum {
            checksum.update(data);
        }
    }

    /// Not used: each verifier is validated on its own, to tell which one
    /// failed. Fails closed if called anyway.
    fn validate(&mut self) -> bool {
        debug_assert!(false, "validate each verifier on its own");
        false
    }
}

/// Write `template` back to `s`, in the syntax [`Template::parse`] reads.
///
/// `s` is cleared first, so that one buffer can be reused for every
/// template.
fn write_template(template: &Template<'_>, s: &mut String) {
    s.clear();
    for item in template.items.iter() {
        match item {
            leon::Item::Text(text) => {
                for c in text.chars() {
                    if matches!(c, '\\' | '{' | '}') {
                        s.push('\\');
                    }
                    s.push(c);
                }
            }
            leon::Item::Key(key) => {
                s.push_str("{ ");
                s.push_str(key);
                s.push_str(" }");
            }
        }
    }
}

/// Other names commonly used for an architecture in release assets, tried
/// after the architecture's own name when `pkg-url` uses `{ target-arch }`.
///
/// Keep this short: each alias is one more request per `pkg-url` candidate.
pub(crate) fn target_arch_aliases(target_arch: &str) -> &'static [&'static str] {
    match target_arch {
        "aarch64" => &["arm64"],
        "x86_64" => &["amd64"],
        _ => &[],
    }
}

/// All URLs `pkg_url` may render to for this target, with the archive suffix
/// and the `{ target-arch }` alias each was rendered with.
fn candidate_urls(
    data: &Data,
    target: &str,
    target_related_info: &dyn leon::Values,
    pkg_fmt: PkgFmt,
    pkg_url: &Template<'_>,
    repo: Option<&str>,
    subcrate: Option<&str>,
) -> Vec<(Url, Option<&'static str>, Option<&'static str>)> {
    let render_url = |ext, target_arch| {
        let mut ctx =
            Context::from_data_with_repo(data, target, target_related_info, ext, repo, subcrate);
        ctx.target_arch = target_arch;
        match ctx.render_url_with(pkg_url) {
            Ok(url) => Some((url, ext, target_arch)),
            Err(err) => {
                warn!("Failed to render url for {ctx:#?}: {err}");
                None
            }
        }
    };

    // `None` renders the architecture's own name, which is tried first.
    let target_arches = iter::once(None).chain(
        pkg_url
            .has_key("target-arch")
            .then(|| target_related_info.get_value("target-arch"))
            .flatten()
            .map(|target_arch| target_arch_aliases(&target_arch))
            .unwrap_or_default()
            .iter()
            .copied()
            .map(Some),
    );

    let exts = if pkg_url.has_any_of_keys(&["format", "archive-format", "archive-suffix"]) {
        // build up list of potential URLs
        Either::Left(
            pkg_fmt
                .extensions(target.contains("windows"))
                .iter()
                .copied()
                .map(Some),
        )
    } else {
        Either::Right(iter::once(None))
    };

    target_arches
        .cartesian_product(exts)
        .filter_map(|(target_arch, ext)| render_url(ext, target_arch))
        .collect()
}

#[async_trait::async_trait]
impl super::Fetcher for GhCrateMeta {
    fn new(
        client: Client,
        gh_api_client: GhApiClient,
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
            resolution: OnceLock::new(),
        })
    }

    fn find(self: Arc<Self>) -> JoinHandle<Result<bool, FetchError>> {
        tokio::spawn(async move {
            let info = self.data.get_repo_info(&self.gh_api_client).await?;

            let repo = info.map(|info| &info.repo);
            let subcrate = info.and_then(|info| info.subcrate.as_deref());
            let is_repo_private = info.map(|info| info.is_private).unwrap_or_default();

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

            let pkg_fmts = if let Some(pkg_fmt) = pkg_fmt {
                Either::Left(iter::once(pkg_fmt))
            } else {
                Either::Right(PkgFmt::iter())
            };

            // Every URL the package may be at, rendered once.
            let mut buf = String::new();
            let candidates: Vec<Candidate> = pkg_urls
                .map(|pkg_url| {
                    write_template(&pkg_url, &mut buf);
                    let pkg_url_str: Arc<str> = buf.as_str().into();
                    (pkg_url_str, pkg_url)
                })
                .cartesian_product(pkg_fmts)
                .flat_map(|((pkg_url_str, pkg_url), pkg_fmt)| {
                    candidate_urls(
                        &self.data,
                        &self.target_data.target,
                        &self.target_data.target_related_info,
                        pkg_fmt,
                        &pkg_url,
                        repo,
                        subcrate,
                    )
                    .into_iter()
                    .map(move |(url, archive_suffix, target_arch)| {
                        Candidate::new(
                            url,
                            pkg_fmt,
                            archive_suffix,
                            target_arch,
                            pkg_url_str.clone(),
                            repo,
                        )
                    })
                })
                .collect();

            let mut probe = Probe::new(
                self.data.clone(),
                self.client.clone(),
                self.gh_api_client.clone(),
                repo,
                subcrate,
                is_repo_private,
                candidates,
            );

            // Try how a previous version was found first, before any other
            // request: normally one URL, instead of all of them. See
            // `Probe::resolve_hint` for when it isn't.
            //
            // Only a hint that is still one of the crate's templates is used,
            // and only if it agrees with the crate's pkg-fmt, if any.
            if let Some(hint) = self.data.pkg_url_hint(&self.target_data.target) {
                if let Some(resolved) = probe.resolve_hint(hint).await? {
                    debug!(
                        ?resolved,
                        "Found package with the pkg-url of the last install"
                    );
                    self.resolution
                        .set(resolved)
                        .expect("find() should be only called once");
                    return Ok(true);
                }

                debug!(
                    "pkg-url {} of the last install did not match, trying all pkg-url",
                    hint.pkg_url
                );
            }

            if let Some(resolved) = probe.resolve().await? {
                debug!(?resolved, "Winning URL found!");
                self.resolution
                    .set(resolved)
                    .expect("find() should be only called once");
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    async fn fetch_and_extract(&self, dst: &Path) -> Result<ExtractedFiles, FetchError> {
        let resolved = self
            .resolution
            .get()
            .expect("find() should be called once before fetch_and_extract()");
        trace!(?resolved, "preparing to fetch");

        let signature = async {
            Ok(
                match (self.signature_policy, &self.target_data.meta.signing) {
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

                        let sign_url = {
                            let mut ctx = Context::from_data_with_repo(
                                &self.data,
                                &self.target_data.target,
                                &self.target_data.target_related_info,
                                resolved.archive_suffix.as_deref(),
                                resolved.repo.as_deref(),
                                resolved.subcrate.as_deref(),
                            );
                            ctx.target_arch = resolved.target_arch;
                            ctx.with_url(&resolved.url).render_url_with(&template)?
                        };

                        debug!(?sign_url, "Downloading signature");
                        let signature = Download::new(self.client.clone(), sign_url)
                            .into_bytes()
                            .await?;
                        trace!(?signature, "got signature contents");

                        SignatureVerifier::new(config, &signature)?
                    }
                },
            )
        };

        // `--skip-signatures` skips checksum files too: it is the way out
        // of a release whose checksum file is wrong.
        let checksum = async {
            if self.signature_policy == SignaturePolicy::Ignore {
                return None;
            }
            checksum::find(
                &self.client,
                &self.gh_api_client,
                &self.data,
                &resolved.url,
                resolved.is_repo_private,
            )
            .await
        };

        let (verifier, checksum) = futures_util::future::join(signature, checksum).await;
        let verifier = verifier?;
        if let Some(checksum) = &checksum {
            debug!(
                "Package must have the {} checksum from {}",
                checksum.algorithm.name(),
                checksum.source
            );
        }

        debug!(
            url=%resolved.url,
            dst=%dst.display(),
            fmt=?resolved.pkg_fmt,
            "Downloading package",
        );
        let mut data_verifier = verifier.data_verifier()?;
        let mut checksum_verifier = checksum.as_ref().map(checksum::Checksum::verifier);
        let mut verifiers = Verifiers {
            signature: data_verifier.as_mut(),
            checksum: checksum_verifier.as_mut(),
        };
        let files = match resolved.gh_release_artifact_url.as_ref() {
            Some(artifact_url) if resolved.is_repo_private => self
                .gh_api_client
                .download_artifact(artifact_url.clone())
                .await?
                .with_data_verifier(&mut verifiers),
            _ => Download::new_with_data_verifier(
                self.client.clone(),
                resolved.url.clone(),
                &mut verifiers,
            ),
        }
        .and_extract(resolved.pkg_fmt, dst)
        .await?;

        if let (Some(checksum), Some(checksum_verifier)) = (&checksum, &mut checksum_verifier) {
            trace!("validating checksum");
            if !checksum_verifier.validate() {
                return Err(FetchError::ChecksumMismatch {
                    url: Box::new(resolved.url.clone()),
                    checksum_source: checksum.source.clone(),
                    algorithm: checksum.algorithm.name(),
                    expected: checksum.expected_hex().into(),
                    actual: checksum_verifier.actual_hex().into(),
                });
            }
            info!(
                "Verified {} checksum of package '{}' from {}",
                checksum.algorithm.name(),
                self.data.name,
                checksum.source
            );
        }

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

    fn resolved_target_arch(&self) -> Option<&str> {
        self.resolution.get()?.target_arch
    }

    fn pkg_url_hint(&self) -> Option<PkgUrlHint> {
        let resolved = self.resolution.get()?;
        Some(PkgUrlHint {
            name: self.data.name.clone(),
            pkg_url: resolved.pkg_url.as_ref().into(),
            pkg_fmt: resolved.pkg_fmt,
            rendered_with: Some(RenderedWith {
                archive_suffix: resolved.archive_suffix.as_deref().map(CompactString::from),
                target_arch: resolved.target_arch.map(CompactString::from),
            }),
        })
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
        FETCHER_GH_CRATE_META
    }

    fn strategy(&self) -> Strategy {
        Strategy::CrateMetaData
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

    /// Replaces `target-arch` from `target_related_info`.
    target_arch: Option<&'c str>,

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

            "target-arch" if self.target_arch.is_some() => self.target_arch.map(Cow::Borrowed),

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
            target_arch: None,

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
    use super::{super::Data, Context, candidate_urls};
    use binstalk_types::cargo_toml_binstall::PkgFmt;
    use compact_str::ToCompactString;
    use leon::Template;
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
    fn target_arch_aliases_are_tried_after_own_name() {
        let data = Data::new(
            "redu".to_compact_string(),
            "0.2.12".to_compact_string(),
            Some("https://github.com/drdo/redu".to_string()),
        );
        let target_info = leon::vals(|key| match key {
            "target-family" => Some("darwin".into()),
            "target-arch" => Some("aarch64".into()),
            _ => None,
        });
        let urls = |template: &str| {
            candidate_urls(
                &data,
                "aarch64-apple-darwin",
                &target_info,
                PkgFmt::Bin,
                &Template::parse(template).unwrap(),
                data.repo.as_deref(),
                None,
            )
            .into_iter()
            .map(|(url, _, target_arch)| (url.to_string(), target_arch))
            .collect::<Vec<_>>()
        };

        let base = "https://github.com/drdo/redu/releases/download/v0.2.12/redu-0.2.12-darwin";
        assert_eq!(
            urls(
                "{ repo }/releases/download/v{ version }/{ name }-{ version }-{ target-family }-{ target-arch }"
            ),
            [
                (format!("{base}-aarch64"), None),
                (format!("{base}-arm64"), Some("arm64")),
            ]
        );

        // Templates without `{ target-arch }` make no extra requests.
        assert_eq!(
            urls("{ repo }/releases/download/v{ version }/{ name }-{ target }"),
            [(
                "https://github.com/drdo/redu/releases/download/v0.2.12/redu-aarch64-apple-darwin"
                    .to_string(),
                None
            )]
        );
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
            "https://github.com/ryankurte/cargo-binstall/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz",
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
            "https://example.com/releases/download/v1.2.3/cargo-binstall-x86_64-unknown-linux-gnu-v1.2.3.tgz",
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
            "https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz",
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
            "https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz",
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
            "https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-apple-darwin.tar.xz",
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
            "https://github.com/watchexec/cargo-watch/releases/download/v9.0.0/cargo-watch-v9.0.0-aarch64-pc-windows-msvc.exe",
        );
    }

    fn template_to_string(template: &Template<'_>) -> String {
        let mut s = "stale".to_owned();
        super::write_template(template, &mut s);
        s
    }

    #[test]
    fn template_to_string_round_trips() {
        use super::hosting::RepositoryHost;

        let hosts = [
            RepositoryHost::GitHub,
            RepositoryHost::GitLab,
            RepositoryHost::BitBucket,
            RepositoryHost::SourceForge,
            RepositoryHost::Codeberg,
        ];
        let mut count = 0;
        for template in hosts
            .into_iter()
            .flat_map(|host| host.get_default_pkg_url_template().unwrap())
        {
            let s = template_to_string(&template);
            assert_eq!(
                template_to_string(&Template::parse(&s).unwrap()),
                s,
                "{template:?}"
            );
            count += 1;
        }
        assert!(count > 0);

        let template = Template::parse(r"\{ { name } \} \\").unwrap();
        let s = template_to_string(&template);
        assert_eq!(s, r"\{ { name } \} \\");
        assert_eq!(
            Template::parse(&s)
                .unwrap()
                .render(&[("name", "a")])
                .unwrap(),
            r"{ a } \"
        );
    }
}
