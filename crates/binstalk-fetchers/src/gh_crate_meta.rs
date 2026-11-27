use std::{borrow::Cow, fmt, iter, path::Path, sync::Arc};

use binstalk_downloader::download::DownloadError;
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
    Data, Extracted, FetchError, Fetched, Found, InvalidPkgFmtError, RepoInfo, SignaturePolicy,
    SignatureVerifier, TargetDataErased, common::*, fetch_context::PackageId,
};

pub const FETCHER_GH_CRATE_META: &str = "GhCrateMeta";

pub(crate) mod hosting;
mod probe;

use hosting::RepositoryHost;
use probe::{Candidate, Probe};

pub struct GhCrateMeta {
    client: Client,
    gh_api_client: GhApiClient,
    data: Arc<Data>,
    target_data: Arc<TargetDataErased>,
    signature_policy: SignaturePolicy,
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
    /// The response to the GET that found `url`, its body unread, which
    /// `fetch_and_extract` downloads instead of asking again.
    response: Option<GetResponse>,
    /// The binary it is the package of, if each binary is in its own
    /// package, see [`Fetched::bin`].
    bin: Option<CompactString>,
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
///
/// * `bin` - the binary to render `{ bin }` as, if any.
#[allow(clippy::too_many_arguments)]
fn candidate_urls(
    data: &Data,
    bin: Option<&str>,
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
        ctx.bin = bin;
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

/// Only the templates without key `subcrate`, unless the crate is in a
/// subdirectory of its repository.
fn filter_subcrate<'t>(
    templates: impl Iterator<Item = Template<'static>> + 't,
    subcrate: Option<&str>,
) -> impl Iterator<Item = Template<'t>> + 't {
    let has_subcrate = subcrate.is_some();
    templates
        .map(Template::cast)
        .filter(move |template| has_subcrate || !template.has_key("subcrate"))
}

/// Every pkg-url, with how it is written for `PkgUrlHint::pkg_url`.
fn with_written_form<'t>(
    pkg_urls: &mut dyn Iterator<Item = Template<'t>>,
) -> Vec<(Arc<str>, Template<'t>)> {
    let mut buf = String::new();
    pkg_urls
        .map(|pkg_url| {
            write_template(&pkg_url, &mut buf);
            (buf.as_str().into(), pkg_url)
        })
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
        _telemetry: bool,
    ) -> Arc<dyn super::Fetcher> {
        Arc::new(Self {
            client,
            gh_api_client,
            data,
            target_data,
            signature_policy,
        })
    }

    async fn find(self: Arc<Self>) -> Result<Option<Box<dyn Found>>, FetchError> {
        let meta = &self.target_data.meta;
        let pkg_url = meta.pkg_url.as_deref().map(Template::parse).transpose()?;

        let info = if needs_repo_info(pkg_url.as_ref(), meta)? {
            self.data.get_repo_info(&self.gh_api_client).await?
        } else {
            None
        };

        let repo = info.map(|info| &info.repo);
        let subcrate = info.and_then(|info| info.subcrate.as_deref());

        let mut pkg_fmt = self.target_data.meta.pkg_fmt;

        let mut is_per_bin = false;
        // Whether the default templates on github.com are used.
        let mut is_default_on_github_com = false;

        let mut pkg_urls = if let Some(template) = pkg_url {
            let pkg_url = meta.pkg_url.as_deref().expect("parsed above");
            is_per_bin = template.has_key("bin");

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
                let crate_name = &self.data.name;
                let version = &self.data.version;
                let target = &self.target_data.target;

                let Some(guessed) = PkgFmt::guess_pkg_format(pkg_url) else {
                    return Err(InvalidPkgFmtError {
                        crate_name: crate_name.clone(),
                        version: version.clone(),
                        target: target.into(),
                        pkg_url: pkg_url.into(),
                        reason: &"pkg-fmt is not specified, yet pkg-url does not contain format, \
                            archive-format or archive-suffix which is required for automatically \
                            deducing pkg-fmt",
                    }
                    .into());
                };
                pkg_fmt = Some(guessed);

                warn!(
                    "Crate {crate_name}@{version} on target {target} does not specify pkg-fmt \
                    but its pkg-url also does not contain key format, archive-format or \
                    archive-suffix.\nbinstall was able to guess that from pkg-url, but \
                    just note that it could be wrong:\npkg-fmt=\"{pkg_fmt}\", pkg-url=\"{pkg_url}\"",
                    pkg_fmt = guessed,
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
                is_default_on_github_com = repo.domain() == Some("github.com");
                Either::Right(filter_subcrate(pkg_urls, subcrate))
            } else {
                warn!(
                    concat!(
                        "Unknown repository {}, cargo-binstall cannot provide default pkg_url for it.\n",
                        "Please ask the upstream to provide it for target {}."
                    ),
                    repo, self.target_data.target
                );

                return Ok(None);
            }
        } else {
            warn!(
                concat!(
                    "Package does not specify repository, cargo-binstall cannot provide default pkg_url for it.\n",
                    "Please ask the upstream to provide it for target {}."
                ),
                self.target_data.target
            );

            return Ok(None);
        };

        // Convert Option<Url> to Option<String> to reduce size of future.
        let repo = repo.map(|u| u.as_str().trim_end_matches('/'));

        let pkg_fmts = if let Some(pkg_fmt) = pkg_fmt {
            Either::Left(iter::once(pkg_fmt))
        } else {
            Either::Right(PkgFmt::iter())
        };

        let pkg_urls = with_written_form(&mut pkg_urls);

        if is_per_bin {
            return self.find_per_bin(&pkg_urls, pkg_fmts, repo, subcrate).await;
        }

        // The default templates for a package named after the only binary
        // to install, if it is named otherwise than the crate.
        //
        // Only on github.com, whose asset lists, fetched for the crate's own
        // templates already, tell whether they exist without a request. On
        // other hosts each would be checked with a request of its own.
        let bin_pkg_urls = match &self.data.bins[..] {
            [bin] if is_default_on_github_com && bin.name != self.data.name => {
                RepositoryHost::GitHub
                    .get_default_bin_pkg_url_template()
                    .map(|pkg_urls| with_written_form(&mut filter_subcrate(pkg_urls, subcrate)))
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };

        // Named after the binary first if that is how a previous version was
        // found, otherwise after the crate first.
        let target = &self.target_data.target;
        let bin_named_first = self.data.pkg_url_hint(target).is_some_and(|hint| {
            bin_pkg_urls
                .iter()
                .any(|(pkg_url, _)| **pkg_url == *hint.pkg_url)
        });

        // Both before any other fetcher, so that the upstream package wins
        // over third-party ones such as cargo-quickinstall's. They are assets
        // of the same releases, whose tags and asset lists the probes share
        // through `Data::context`: no more requests are needed.
        for bin_named in [bin_named_first, !bin_named_first] {
            let found = if bin_named {
                if bin_pkg_urls.is_empty() {
                    continue;
                }
                debug!(
                    "Looking for the package of {} named after its binary on target {target}",
                    self.data.name
                );
                self.find_per_bin(&bin_pkg_urls, pkg_fmts.clone(), repo, subcrate)
                    .await?
            } else {
                let candidates = self.candidates(&pkg_urls, pkg_fmts.clone(), None, repo, subcrate);
                self.probe(candidates, repo, subcrate)
                    .await?
                    .map(|resolved| self.found(resolved))
            };
            if found.is_some() {
                return Ok(found);
            }
        }

        Ok(None)
    }

    fn fetcher_name(&self) -> &'static str {
        FETCHER_GH_CRATE_META
    }

    fn strategy(&self) -> Strategy {
        Strategy::CrateMetaData
    }
}

impl GhCrateMeta {
    fn found(self: &Arc<Self>, resolved: Resolved) -> Box<dyn Found> {
        Box::new(GhCrateMetaFound {
            fetcher: Arc::clone(self),
            resolved,
        })
    }

    /// Every URL the package may be at, rendered once.
    ///
    /// * `bin` - the binary to render `{ bin }` as, if any.
    fn candidates(
        &self,
        pkg_urls: &[(Arc<str>, Template<'_>)],
        pkg_fmts: impl Iterator<Item = PkgFmt> + Clone,
        bin: Option<&str>,
        repo: Option<&str>,
        subcrate: Option<&str>,
    ) -> Vec<Candidate> {
        pkg_urls
            .iter()
            .cartesian_product(pkg_fmts)
            .flat_map(|((pkg_url_str, pkg_url), pkg_fmt)| {
                candidate_urls(
                    &self.data,
                    bin,
                    &self.target_data.target,
                    &self.target_data.target_related_info,
                    pkg_fmt,
                    pkg_url,
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
                    )
                })
            })
            .collect()
    }

    /// Check `candidates`, returning the first one found.
    async fn probe(
        &self,
        candidates: Vec<Candidate>,
        repo: Option<&str>,
        subcrate: Option<&str>,
    ) -> Result<Option<Resolved>, FetchError> {
        let mut probe = Probe::new(
            self.data.clone(),
            self.client.clone(),
            self.gh_api_client.clone(),
            repo,
            subcrate,
            candidates,
        )
        .await;

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
                return Ok(Some(resolved));
            }

            debug!(
                "pkg-url {} of the last install did not match, trying all pkg-url",
                hint.pkg_url
            );
        }

        let resolved = probe.resolve().await?;
        if let Some(resolved) = &resolved {
            debug!(?resolved, "Winning URL found!");
        }
        Ok(resolved)
    }

    /// Find the package of each binary of [`Data::with_bins`], for a
    /// `pkg-url` that contains key `bin`, or the default templates for a
    /// package named after the binary.
    ///
    /// Finds nothing if the package of a binary that isn't optional is
    /// missing.
    async fn find_per_bin(
        self: &Arc<Self>,
        pkg_urls: &[(Arc<str>, Template<'_>)],
        pkg_fmts: impl Iterator<Item = PkgFmt> + Clone,
        repo: Option<&str>,
        subcrate: Option<&str>,
    ) -> Result<Option<Box<dyn Found>>, FetchError> {
        let bins = &self.data.bins;
        let target = &self.target_data.target;

        if bins.is_empty() {
            debug!("No binary is specified to find the packages of, skipping");
            return Ok(None);
        }

        // Look for the packages of all binaries at once: they are usually
        // assets of the same release, whose lookups the probes share
        // through `Data::context`.
        let repo_owned = repo.map(CompactString::from);
        let subcrate_owned = subcrate.map(CompactString::from);
        // Dropping the rest, once one is missing, aborts them.
        let handles: Vec<_> = bins
            .iter()
            .map(|bin| {
                let candidates =
                    self.candidates(pkg_urls, pkg_fmts.clone(), Some(&bin.name), repo, subcrate);
                let this = Arc::clone(self);
                let repo = repo_owned.clone();
                let subcrate = subcrate_owned.clone();
                AutoAbortJoinHandle::spawn(async move {
                    this.probe(candidates, repo.as_deref(), subcrate.as_deref())
                        .await
                })
            })
            .collect();

        let mut found = Vec::with_capacity(bins.len());
        let mut missing_optional = Vec::new();

        for (bin, handle) in bins.iter().zip(handles) {
            if let Some(mut resolved) = handle.flattened_join().await? {
                resolved.bin = Some(bin.name.clone());
                found.push(GhCrateMetaFound {
                    fetcher: Arc::clone(self),
                    resolved,
                });
            } else if bin.optional {
                missing_optional.push(&bin.name);
            } else {
                debug!(
                    "The package of {} bin {} is not found on target {target}",
                    self.data.name, bin.name
                );
                return Ok(None);
            }
        }

        if found.is_empty() {
            return Ok(None);
        }

        for bin in missing_optional {
            warn!(
                "The package of {} bin {bin} is not found on target {target}. \
But since the bin is optional, it is ignored.",
                self.data.name,
            );
        }

        Ok(Some(Box::new(PerBinFound(found))))
    }
}

/// The package [`GhCrateMeta`] found.
struct GhCrateMetaFound {
    fetcher: Arc<GhCrateMeta>,
    resolved: Resolved,
}

impl GhCrateMetaFound {
    /// What [`GhCrateMetaFound::download_and_extract`] downloads, and how
    /// it verifies it.
    fn package_id(&self) -> Result<PackageId, FetchError> {
        let Self { fetcher, resolved } = self;
        // The same verifier as `download_and_extract`.
        let signing = match (fetcher.signature_policy, &fetcher.target_data.meta.signing) {
            (SignaturePolicy::Ignore, _) | (SignaturePolicy::IfPresent, None) => None,
            (SignaturePolicy::Require, None) => return Err(FetchError::MissingSignature),
            (_, Some(config)) => Some((config.algorithm, config.pubkey.as_ref().into())),
        };
        Ok(PackageId {
            url: resolved.url.clone(),
            pkg_fmt: resolved.pkg_fmt,
            signing,
        })
    }

    async fn download_and_extract(&mut self, dst: &Path) -> Result<ExtractedFiles, FetchError> {
        let Self { fetcher, resolved } = self;
        trace!(?resolved, "preparing to fetch");

        let verifier = match (fetcher.signature_policy, &fetcher.target_data.meta.signing) {
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
                        &fetcher.data,
                        &fetcher.target_data.target,
                        &fetcher.target_data.target_related_info,
                        resolved.archive_suffix.as_deref(),
                        resolved.repo.as_deref(),
                        resolved.subcrate.as_deref(),
                    );
                    ctx.target_arch = resolved.target_arch;
                    ctx.bin = resolved.bin.as_deref();
                    ctx.with_url(&resolved.url).render_url_with(&template)?
                };

                debug!(?sign_url, "Downloading signature");
                let signature = Download::new(fetcher.client.clone(), sign_url)
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
        let files = match resolved.gh_release_artifact_url.as_ref() {
            Some(artifact_url) if resolved.is_repo_private => fetcher
                .gh_api_client
                .download_artifact(artifact_url.clone())
                .await?
                .with_data_verifier(data_verifier.as_mut()),
            _ => resolved
                .response
                .take()
                .map(Download::from)
                .unwrap_or_else(|| Download::new(fetcher.client.clone(), resolved.url.clone()))
                .with_data_verifier(data_verifier.as_mut()),
        }
        .and_extract(resolved.pkg_fmt, dst)
        .await?;
        trace!("validating signature (if any)");
        if data_verifier.validate() {
            if let Some(info) = verifier.info() {
                match &resolved.bin {
                    Some(bin) => info!(
                        "Verified signature for package of '{}' bin '{bin}': {info}",
                        fetcher.data.name
                    ),
                    None => info!(
                        "Verified signature for package '{}': {info}",
                        fetcher.data.name
                    ),
                }
            }
            Ok(files)
        } else {
            Err(FetchError::InvalidSignature)
        }
    }
}

impl GhCrateMetaFound {
    /// Fetch and extract it to `dst`, unless it was for another crate.
    async fn extract(&mut self, dst: &Path) -> Result<Arc<Extracted>, FetchError> {
        let id = self.package_id()?;
        let context = Arc::clone(&self.fetcher.data.context);
        context
            .extract_once(id, dst, async || self.download_and_extract(dst).await)
            .await
    }
}

#[async_trait::async_trait]
impl Found for GhCrateMetaFound {
    async fn fetch_and_extract(&mut self, dst: &Path) -> Result<Vec<Fetched>, FetchError> {
        Ok(vec![Fetched {
            bin: self.resolved.bin.clone(),
            meta: self.target_meta(),
            extracted: self.extract(dst).await?,
        }])
    }

    fn pkg_fmt(&self) -> PkgFmt {
        self.resolved.pkg_fmt
    }

    fn target_meta(&self) -> PkgMeta {
        let mut meta = self.fetcher.target_data.meta.clone();
        meta.pkg_fmt = Some(self.pkg_fmt());
        meta
    }

    fn resolved_target_arch(&self) -> Option<&str> {
        self.resolved.target_arch
    }

    fn pkg_url_hint(&self) -> Option<PkgUrlHint> {
        let resolved = &self.resolved;
        Some(PkgUrlHint {
            name: self.fetcher.data.name.clone(),
            pkg_url: resolved.pkg_url.as_ref().into(),
            pkg_fmt: resolved.pkg_fmt,
            rendered_with: Some(RenderedWith {
                archive_suffix: resolved.archive_suffix.as_deref().map(CompactString::from),
                target_arch: resolved.target_arch.map(CompactString::from),
            }),
        })
    }

    fn is_third_party(&self) -> bool {
        false
    }

    fn target(&self) -> &str {
        &self.fetcher.target_data.target
    }

    fn target_data(&self) -> &Arc<TargetDataErased> {
        &self.fetcher.target_data
    }

    fn source_name(&self) -> CompactString {
        let url = &self.resolved.url;
        if let Some(domain) = url.domain() {
            domain.to_compact_string()
        } else if let Some(host) = url.host_str() {
            host.to_compact_string()
        } else {
            url.to_compact_string()
        }
    }
}

/// Whether the crate's repository is needed: for the default templates, and
/// to render `{ repo }` and `{ subcrate }` in `pkg-url` or the signature
/// file, which go by the URL it redirects to. A `pkg-url` pointing elsewhere,
/// e.g. at cargo-quickinstall, needs no request for it: each candidate is
/// checked in its own repository, see [`Probe`].
fn needs_repo_info(pkg_url: Option<&Template<'_>>, meta: &PkgMeta) -> Result<bool, FetchError> {
    let uses_repo = |template: &Template<'_>| template.has_any_of_keys(&["repo", "subcrate"]);
    let Some(pkg_url) = pkg_url else {
        return Ok(true);
    };
    if uses_repo(pkg_url) {
        return Ok(true);
    }
    match meta
        .signing
        .as_ref()
        .and_then(|signing| signing.file.as_deref())
    {
        Some(file) => Ok(uses_repo(&Template::parse(file)?)),
        None => Ok(false),
    }
}

/// The packages [`GhCrateMeta`] found when each binary is in its own
/// package, see [`Fetched::bin`].
///
/// Never empty. Everything but the packages themselves is the same for all
/// of them, and is taken from the first.
struct PerBinFound(Vec<GhCrateMetaFound>);

impl PerBinFound {
    fn first(&self) -> &GhCrateMetaFound {
        &self.0[0]
    }
}

#[async_trait::async_trait]
impl Found for PerBinFound {
    /// Each package is extracted to `dst/BIN`.
    async fn fetch_and_extract(&mut self, dst: &Path) -> Result<Vec<Fetched>, FetchError> {
        std::fs::create_dir_all(dst).map_err(DownloadError::from)?;

        let mut packages = Vec::with_capacity(self.0.len());
        for found in &mut self.0 {
            let bin = found
                .resolved
                .bin
                .as_deref()
                .expect("PerBinFound is only made of packages of a binary");
            packages.extend(found.fetch_and_extract(&dst.join(bin)).await?);
        }
        Ok(packages)
    }

    fn pkg_fmt(&self) -> PkgFmt {
        self.first().pkg_fmt()
    }

    fn target_meta(&self) -> PkgMeta {
        self.first().target_meta()
    }

    fn resolved_target_arch(&self) -> Option<&str> {
        self.first().resolved_target_arch()
    }

    /// The pkg-url is the same for all binaries, so the hint is too.
    fn pkg_url_hint(&self) -> Option<PkgUrlHint> {
        self.first().pkg_url_hint()
    }

    fn is_third_party(&self) -> bool {
        false
    }

    fn target(&self) -> &str {
        self.first().target()
    }

    fn target_data(&self) -> &Arc<TargetDataErased> {
        self.first().target_data()
    }

    fn source_name(&self) -> CompactString {
        self.first().source_name()
    }
}

/// Template for constructing download paths
#[derive(Clone)]
struct Context<'c> {
    name: &'c str,
    repo: Option<&'c str>,
    target: &'c str,
    version: &'c str,

    /// Name of the binary, only set when each binary is in its own package.
    bin: Option<&'c str>,

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
            .field("bin", &self.bin)
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
            "bin" => self.bin.map(Cow::Borrowed),

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
            bin: None,
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
            Default::default(),
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
                None,
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
                Default::default(),
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
                Default::default(),
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
                Default::default(),
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
                Default::default(),
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
                Default::default(),
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
    fn bin() {
        let data = Data::new(
            Default::default(),
            "flowr".to_compact_string(),
            "0.142.0".to_compact_string(),
            Some("https://github.com/andrewdavidmackenzie/flow".to_string()),
        );
        let target_info = leon::vals(|_| None);
        let mut ctx = Context::from_data_with_repo(
            &data,
            "x86_64-unknown-linux-gnu",
            &target_info,
            Some(".tgz"),
            data.repo.as_deref(),
            None,
        );
        ctx.bin = Some("flowrcli");

        assert_eq!(
            ctx.render_url(
                "{ repo }/releases/download/v{ version }/{ bin }-{ target }.{ archive-format }"
            )
            .unwrap()
            .as_str(),
            "https://github.com/andrewdavidmackenzie/flow/releases/download/v0.142.0/flowrcli-x86_64-unknown-linux-gnu.tgz",
        );
    }

    #[test]
    fn bin_without_binary() {
        let data = Data::new(
            Default::default(),
            "flowr".to_compact_string(),
            "0.142.0".to_compact_string(),
            Some("https://github.com/andrewdavidmackenzie/flow".to_string()),
        );
        let target_info = leon::vals(|_| None);
        let ctx = Context::from_data_with_repo(
            &data,
            "x86_64-unknown-linux-gnu",
            &target_info,
            Some(".tgz"),
            data.repo.as_deref(),
            None,
        );

        ctx.render_url("{ repo }/releases/download/v{ version }/{ bin }.tgz")
            .unwrap_err();
    }

    #[test]
    fn no_archive() {
        assert_context_rendering(
            &Data::new(
                Default::default(),
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

    #[test]
    fn repo_info_is_needed() {
        use crate::gh_crate_meta::{PkgMeta, needs_repo_info};
        use binstalk_types::cargo_toml_binstall::{PkgSigning, SigningAlgorithm};

        let needs = |pkg_url: Option<&str>, sig_file: Option<&str>| {
            let meta = PkgMeta {
                signing: sig_file.map(|file| PkgSigning {
                    algorithm: SigningAlgorithm::Minisign,
                    pubkey: "".into(),
                    file: Some(file.to_owned()),
                }),
                ..Default::default()
            };
            let pkg_url = pkg_url.map(|pkg_url| Template::parse(pkg_url).unwrap());
            needs_repo_info(pkg_url.as_ref(), &meta).unwrap()
        };
        const QUICKINSTALL: &str = "https://github.com/cargo-bins/cargo-quickinstall/releases/download/{ name }-{ version }/{ name }-{ version }-{ target }.tar.gz";

        // The default templates.
        assert!(needs(None, None));
        assert!(needs(
            Some("{ repo }/releases/download/v{ version }/a.tgz"),
            None
        ));
        assert!(needs(Some("https://a.example/{ subcrate }/a.tgz"), None));
        assert!(!needs(Some(QUICKINSTALL), None));
        assert!(!needs(Some(QUICKINSTALL), Some("{ url }.sig")));
        assert!(needs(
            Some(QUICKINSTALL),
            Some("{ repo }/releases/download/a.sig")
        ));
    }

    /// A crate whose packages are named after its binary, not after it, is
    /// found under the binary's name by the default templates.
    #[tokio::test]
    async fn bin_named_package() {
        use std::{
            num::{NonZeroU16, NonZeroU64},
            sync::Arc,
        };

        use binstalk_types::crate_info::PkgUrlHint;

        use crate::{
            BinInfo, FetchContext, Fetcher, GhCrateMeta, SignaturePolicy, TargetData,
            common::{Client, GhApiClient},
        };

        let client = Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
            NonZeroU16::new(300).unwrap(),
            NonZeroU64::new(1).unwrap(),
            [],
        )
        .unwrap();
        let gh_api_client = GhApiClient::new(client.clone(), None);
        let target_data = Arc::new(TargetData {
            target: "x86_64-unknown-linux-musl".into(),
            meta: Default::default(),
            target_related_info: leon::vals(|_| None),
        });

        // cargo-binstall's own releases, looked for as another crate's.
        let find = |context: &Arc<FetchContext>, bins: &[&str], hint: Option<PkgUrlHint>| {
            let data = Data::new(
                context.clone(),
                "bin-named-package-test".into(),
                "1.28.0".into(),
                Some("https://github.com/cargo-bins/cargo-binstall".into()),
            )
            .with_bins(
                bins.iter()
                    .map(|&name| BinInfo {
                        name: name.into(),
                        optional: false,
                    })
                    .collect(),
            );
            let data = match hint {
                Some(hint) => data.with_pkg_url_hint(target_data.target.as_str().into(), hint),
                None => data,
            };
            GhCrateMeta::new(
                client.clone(),
                gh_api_client.clone(),
                Arc::new(data),
                target_data.clone(),
                SignaturePolicy::IfPresent,
                false,
            )
            .find()
        };

        let found = find(&Default::default(), &["cargo-binstall"], None)
            .await
            .unwrap()
            .unwrap();
        let hint = found.pkg_url_hint().unwrap();
        assert_eq!(hint.name, "bin-named-package-test");
        assert_eq!(
            hint.pkg_url,
            "{ repo }/releases/download/v{ version }/{ bin }-{ target }{ archive-suffix }"
        );

        // Found that way again, it is looked for under the binary's name
        // first: the asset list of the release isn't needed.
        let context = Arc::<FetchContext>::default();
        let found = find(&context, &["cargo-binstall"], Some(hint.clone()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.pkg_url_hint().unwrap(), hint);
        assert_eq!(context.release_assets_pages_len(), 0);
        assert_eq!(context.release_artifacts_len(), 0);

        // Not for a crate with more than one binary.
        assert!(
            find(&Default::default(), &["cargo-binstall", "other"], None)
                .await
                .unwrap()
                .is_none()
        );

        // Without the binary, nothing is found.
        assert!(
            find(&Default::default(), &["other"], None)
                .await
                .unwrap()
                .is_none()
        );
    }
}
