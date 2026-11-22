use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap, HashSet},
    fmt, iter,
    path::Path,
    sync::{Arc, OnceLock},
};

use binstalk_git_repo_api::{
    gh_api_client::{
        GhApiError, GhRelease, GhReleaseArtifact, GhReleaseArtifactUrl, ReleaseAssets,
    },
    git_refs::{self, RepoTags},
};
use binstalk_types::{cargo_toml_binstall::Strategy, crate_info::PkgUrlHint};
use compact_str::{CompactString, ToCompactString};
use either::Either;
use itertools::Itertools;
use leon::Template;
use strum::IntoEnumIterator;
use tokio::time::sleep;
use tracing::{debug, info, trace, warn};
use url::Url;

use crate::{
    Data, FetchError, InvalidPkgFmtError, RepoInfo, SignaturePolicy, SignatureVerifier,
    TargetDataErased, common::*, futures_resolver::FuturesResolver, gh_api_rate_limit_wait,
};

pub const FETCHER_GH_CRATE_META: &str = "GhCrateMeta";

pub(crate) mod hosting;

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
    repo: Option<String>,
    subcrate: Option<String>,
    gh_release_artifact_url: Option<GhReleaseArtifactUrl>,
    is_repo_private: bool,
    /// The alias from [`target_arch_aliases`] that `{ target-arch }` was
    /// rendered as in `url`, if not the architecture's own name.
    target_arch: Option<&'static str>,
    /// The default `pkg-url` that `url` was rendered from, see
    /// [`PkgUrlHint::pkg_url`]. `None` if the crate specified `pkg-url`.
    default_pkg_url: Option<CompactString>,
}

/// Write `template` back in the syntax [`Template::parse`] reads.
fn template_to_string(template: &Template<'_>) -> CompactString {
    let mut s = CompactString::default();
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
    s
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

/// What is known about the repository and the releases of the candidates
/// before checking them.
struct KnownRepo<'a> {
    is_private: bool,
    /// Which release tags exist, from the repository's git server.
    tags: Option<&'a RepoTags>,
    /// The candidate asset names of each GitHub release.
    gh_candidates: HashMap<GhRelease, Arc<HashSet<CompactString>>>,
}

/// Check whether `artifact` of a public repository exists on github.com,
/// without the API. Returns `None` if github.com can't tell.
///
/// The release checks are cached per release, so this costs at most two
/// requests per release however many candidates are checked, plus one for each
/// candidate found.
async fn check_on_web(
    client: &Client,
    gh_api_client: &GhApiClient,
    artifact: &GhReleaseArtifact,
    url: &Url,
    // Whether the release's tag exists, if known.
    tag_exists: Option<bool>,
    // The other candidates of the release.
    release_candidates: Option<&HashSet<CompactString>>,
) -> Result<Option<bool>, FetchError> {
    // HEAD the release page first: github.com answers a missing release with
    // a 404 page about as large as an asset list.
    if tag_exists.is_none() && !gh_api_client.has_release_page(&artifact.release).await {
        return Ok(Some(false));
    }

    let assets = match gh_api_client.release_assets_on_web(&artifact.release).await {
        ReleaseAssets::NotFound => return Ok(Some(false)),
        ReleaseAssets::Found(assets) => assets,
        ReleaseAssets::Unknown => return Ok(None),
    };

    if assets.contains(&artifact.artifact_name) {
        // The list is parsed from a page meant for people: make sure a listed
        // asset can be downloaded before picking it.
        return Ok(Some(Box::pin(client.remote_gettable(url.clone())).await?));
    }

    // The page might not list every asset. Only trust that this one is
    // missing if the page lists another candidate of the release, otherwise
    // check some other way.
    Ok(release_candidates
        .is_some_and(|candidates| candidates.iter().any(|name| assets.contains(name)))
        .then_some(false))
}

impl GhCrateMeta {
    fn launch_baseline_find_tasks(
        &self,
        futures_resolver: &FuturesResolver<Resolved, FetchError>,
        pkg_fmt: PkgFmt,
        pkg_url: &Template<'_>,
        repo: Option<&str>,
        subcrate: Option<&str>,
        known: &KnownRepo<'_>,
    ) {
        // `pkg_url` is one of the defaults unless the crate specified it.
        let default_pkg_url = self
            .target_data
            .meta
            .pkg_url
            .is_none()
            .then(|| template_to_string(pkg_url));

        let urls = candidate_urls(
            &self.data,
            &self.target_data.target,
            &self.target_data.target_related_info,
            pkg_fmt,
            pkg_url,
            repo,
            subcrate,
        );

        let is_repo_private = known.is_private;

        // go check all potential URLs at once
        futures_resolver.extend(urls.into_iter().map(move |(url, ext, target_arch)| {
            let client = self.client.clone();
            let gh_api_client = self.gh_api_client.clone();

            let repo = repo.map(ToString::to_string);
            let subcrate = subcrate.map(ToString::to_string);
            let archive_suffix = ext.map(ToString::to_string);
            let gh_release_artifact = GhReleaseArtifact::try_extract_from_url(&url);
            let default_pkg_url = default_pkg_url.clone();

            let tag_exists = repo
                .as_deref()
                .zip(known.tags)
                .and_then(|(repo, tags)| tags.exists(&git_refs::release_tag(repo, &url)?));
            let release_candidates = gh_release_artifact
                .as_ref()
                .and_then(|artifact| known.gh_candidates.get(&artifact.release).cloned());

            async move {
                debug!("Checking for package at: '{url}'");

                if tag_exists == Some(false) {
                    return Ok(None);
                }

                let mut resolved = Resolved {
                    url: url.clone(),
                    pkg_fmt,
                    repo,
                    subcrate,
                    archive_suffix,
                    is_repo_private,
                    gh_release_artifact_url: None,
                    target_arch,
                    default_pkg_url,
                };

                if let Some(artifact) = gh_release_artifact {
                    // Without a token, the API allows 60 requests per hour:
                    // look on github.com first, and only ask the API if that
                    // can't tell.
                    if !is_repo_private
                        && !gh_api_client.has_gh_token()
                        && let Some(found) = check_on_web(
                            &client,
                            &gh_api_client,
                            &artifact,
                            &url,
                            tag_exists,
                            release_candidates.as_deref(),
                        )
                        .await?
                    {
                        return Ok(found.then_some(resolved));
                    }

                    loop {
                        match get_gh_release_artifact_url(gh_api_client.clone(), artifact.clone())
                            .await
                        {
                            Ok(Some(artifact_url)) => {
                                resolved.gh_release_artifact_url = Some(artifact_url);
                                return Ok(Some(resolved));
                            }
                            Ok(None) => return Ok(None),

                            // A public release asset can be checked with
                            // HEAD/GET instead, as `does_url_exist` does.
                            Err(GhApiError::RateLimit { .. } | GhApiError::Unauthorized)
                                if !is_repo_private =>
                            {
                                // One or two requests per release on
                                // github.com, instead of one HEAD/GET per
                                // candidate.
                                if let Some(found) = check_on_web(
                                    &client,
                                    &gh_api_client,
                                    &artifact,
                                    &url,
                                    tag_exists,
                                    release_candidates.as_deref(),
                                )
                                .await?
                                {
                                    return Ok(found.then_some(resolved));
                                }
                                break;
                            }
                            Err(GhApiError::RateLimit { retry_after }) => {
                                match gh_api_rate_limit_wait(retry_after) {
                                    Some(wait) => sleep(wait).await,
                                    None => {
                                        return Err(GhApiError::RateLimit { retry_after }.into());
                                    }
                                }
                            }

                            Err(err) => return Err(err.into()),
                        }
                    }
                }

                Ok(Box::pin(client.remote_gettable(url))
                    .await?
                    .then_some(resolved))
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

            let (pkg_urls, is_default_pkg_url) = if let Some(pkg_url) =
                self.target_data.meta.pkg_url.as_deref()
            {
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

                (Either::Left(iter::once(template)), false)
            } else if let Some(RepoInfo {
                repo,
                repository_host,
                ..
            }) = info
            {
                if let Some(pkg_urls) = repository_host.get_default_pkg_url_template() {
                    let has_subcrate = subcrate.is_some();

                    (
                        Either::Right(
                            pkg_urls
                                .map(Template::cast)
                                // If subcrate is Some, then all templates will be included.
                                // Otherwise, only templates without key "subcrate" will be
                                // included.
                                .filter(move |template| {
                                    has_subcrate || !template.has_key("subcrate")
                                }),
                        ),
                        true,
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

            // Every candidate URL, of every template.
            let all_candidate_urls: Vec<Url> = pkg_urls
                .clone()
                .cartesian_product(pkg_fmts.clone())
                .flat_map(|(pkg_url, pkg_fmt)| {
                    candidate_urls(
                        &self.data,
                        &self.target_data.target,
                        &self.target_data.target_related_info,
                        pkg_fmt,
                        &pkg_url,
                        repo,
                        subcrate,
                    )
                })
                .map(|(url, ..)| url)
                .collect();

            // Before any API call, find out which of the release tags exist
            // with one request to the repository's git server, for all
            // targets. A private repository needs authentication.
            let tags = match repo {
                Some(repo) if !is_repo_private => {
                    self.data
                        .get_repo_tags(&self.client, repo, &all_candidate_urls)
                        .await
                }
                _ => None,
            };
            let known = KnownRepo {
                is_private: is_repo_private,
                tags,
                gh_candidates: all_candidate_urls
                    .iter()
                    .filter_map(GhReleaseArtifact::try_extract_from_url)
                    .filter(|artifact| {
                        tags.and_then(|tags| tags.exists(&artifact.release.tag)) != Some(false)
                    })
                    .map(|artifact| (artifact.release, artifact.artifact_name))
                    .into_grouping_map()
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .map(|(release, names)| (release, Arc::new(names)))
                    .collect(),
            };

            // Try how a previous version was found first: it is one template
            // instead of all of them, times every pkg-fmt.
            //
            // Only a hint that is still one of the default templates is used,
            // and only if it agrees with the crate's pkg-fmt, if any.
            if is_default_pkg_url
                && let Some(PkgUrlHint {
                    pkg_url: hint_pkg_url,
                    pkg_fmt: hint_pkg_fmt,
                    ..
                }) = self.data.pkg_url_hint(&self.target_data.target)
                && pkg_fmt.is_none_or(|pkg_fmt| pkg_fmt == *hint_pkg_fmt)
            {
                if let Some(pkg_url) = pkg_urls
                    .clone()
                    .find(|pkg_url| template_to_string(pkg_url) == *hint_pkg_url)
                {
                    let resolver = FuturesResolver::default();
                    this.launch_baseline_find_tasks(
                        &resolver,
                        *hint_pkg_fmt,
                        &pkg_url,
                        repo,
                        subcrate,
                        &known,
                    );

                    if let Some(resolved) = resolver.resolve().await {
                        debug!(
                            ?resolved,
                            "Found package with the pkg-url of the last install"
                        );
                        self.resolution
                            .set(resolved)
                            .expect("find() should be only called once");
                        return Ok(true);
                    }
                }

                debug!(
                    "pkg-url {hint_pkg_url} of the last install did not match, trying all default pkg-url"
                );
            }

            // All candidates in the same GitHub repository are usually spread
            // over a few release tags. Ask about the ones that exist in one
            // GraphQL query instead of one query per tag.
            if self.gh_api_client.has_gh_token() {
                let releases = known
                    .gh_candidates
                    .keys()
                    .map(|release| (release.repo.clone(), release.tag.clone()))
                    .into_grouping_map()
                    .collect::<BTreeSet<_>>();

                // Normally all candidates are in the same repository.
                for (gh_repo, tags) in releases {
                    self.gh_api_client
                        .prefetch_release_artifacts(&gh_repo, tags)
                        .await;
                }
            }

            let resolver = FuturesResolver::default();

            // Iterate over pkg_urls first to avoid String::clone.
            for pkg_url in pkg_urls {
                //             Clone iter pkg_fmts to ensure all pkg_fmts is
                //             iterated over for each pkg_url, which is
                //             basically cartesian product.
                //             |
                for pkg_fmt in pkg_fmts.clone() {
                    this.launch_baseline_find_tasks(
                        &resolver, pkg_fmt, &pkg_url, repo, subcrate, &known,
                    );
                }
            }

            if let Some(resolved) = resolver.resolve().await {
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
        };

        debug!(
            url=%resolved.url,
            dst=%dst.display(),
            fmt=?resolved.pkg_fmt,
            "Downloading package",
        );
        let mut data_verifier = verifier.data_verifier()?;
        let files = match resolved.gh_release_artifact_url.as_ref() {
            Some(artifact_url) if resolved.is_repo_private => self
                .gh_api_client
                .download_artifact(artifact_url.clone())
                .await?
                .with_data_verifier(data_verifier.as_mut()),
            _ => Download::new_with_data_verifier(
                self.client.clone(),
                resolved.url.clone(),
                data_verifier.as_mut(),
            ),
        }
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

    fn resolved_target_arch(&self) -> Option<&str> {
        self.resolution.get()?.target_arch
    }

    fn pkg_url_hint(&self) -> Option<PkgUrlHint> {
        let resolved = self.resolution.get()?;
        Some(PkgUrlHint {
            name: self.data.name.clone(),
            pkg_url: resolved.default_pkg_url.clone()?,
            pkg_fmt: resolved.pkg_fmt,
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

    #[test]
    fn template_to_string_round_trips() {
        use super::{hosting::RepositoryHost, template_to_string};

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
