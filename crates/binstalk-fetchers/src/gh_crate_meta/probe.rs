//! Checking which of the URLs a package may be at exists, with as few
//! requests as possible.
//!
//! [`Candidate`]s are rendered once per target. The ones of the `pkg-url` a
//! previous version was found with are checked first, see
//! [`Probe::resolve_hint`]. The others are narrowed down before any of them
//! is checked, see [`Probe::resolve`]: by which release tags exist, with one
//! request to the repository's git server, then for a public GitHub
//! repository by the releases' asset lists on github.com, which cost no API
//! quota, and only then with the API.
//!
//! Each candidate is checked in the repository it is in, which is not
//! always the crate's: a `pkg-url` may point at another one, such as
//! cargo-quickinstall's.

use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

use binstalk_git_repo_api::{
    gh_api_client::{
        GhApiError, GhRelease, GhReleaseArtifact, GhReleaseArtifactUrl, GhRepo, ReleaseAssets,
        ReleaseAssetsPage,
    },
    git_refs::{self, ReleaseTag},
};
use compact_str::CompactString;
use itertools::Itertools;
use tokio::time::sleep;
use url::Url;

use binstalk_types::crate_info::PkgUrlHint;

use super::Resolved;
use crate::{
    Data, FetchError, RepoInfo, common::*, futures_resolver::FuturesResolver,
    gh_api_rate_limit_wait,
};

/// A URL the package may be at, rendered from a `pkg-url` template.
#[derive(Debug)]
pub(super) struct Candidate {
    pub(super) url: Url,
    pub(super) pkg_fmt: PkgFmt,
    pub(super) archive_suffix: Option<&'static str>,
    /// The alias from [`super::target_arch_aliases`] that `{ target-arch }`
    /// was rendered as, if not the architecture's own name.
    pub(super) target_arch: Option<&'static str>,
    /// The `pkg-url` it was rendered from, see [`PkgUrlHint::pkg_url`].
    pub(super) pkg_url: Arc<str>,
    /// The release it is an asset of, if known.
    release_tag: Option<ReleaseTag>,
    /// The GitHub release asset it is.
    gh_artifact: Option<GhReleaseArtifact>,
    /// Whether it is in a private GitHub repository, see
    /// [`Probe::find_repos`].
    is_private: bool,
}

impl Candidate {
    pub(super) fn new(
        url: Url,
        pkg_fmt: PkgFmt,
        archive_suffix: Option<&'static str>,
        target_arch: Option<&'static str>,
        pkg_url: Arc<str>,
    ) -> Self {
        Self {
            release_tag: ReleaseTag::of_asset(&url),
            gh_artifact: GhReleaseArtifact::try_extract_from_url(&url),
            is_private: false,
            url,
            pkg_fmt,
            archive_suffix,
            target_arch,
            pkg_url,
        }
    }

    /// Move the release asset from repository `from`, its
    /// [`ReleaseTag::repo`], to `to`, which `from` redirects to.
    fn move_to(&mut self, from: &Url, to: &Url) {
        let path = self.url.path();
        let Some(rest) = path
            .get(..from.path().len())
            .filter(|repo_path| repo_path.eq_ignore_ascii_case(from.path()))
            .map(|_| &path[from.path().len()..])
        else {
            return;
        };
        let mut url = to.clone();
        url.set_path(&format!("{}{rest}", to.path().trim_end_matches('/')));
        url.set_query(self.url.query());
        debug!("{} redirects to {url}", self.url);
        self.release_tag = ReleaseTag::of_asset(&url);
        self.gh_artifact = GhReleaseArtifact::try_extract_from_url(&url);
        self.url = url;
    }
}

/// Whether `a` and `b` are the same repository. github.com and others
/// ignore the case of its path.
fn same_repo(a: &Url, b: &Url) -> bool {
    a.as_str()
        .trim_end_matches('/')
        .eq_ignore_ascii_case(b.as_str().trim_end_matches('/'))
}

/// What [`Probe::find_repos`] needs of a [`RepoInfo`].
#[derive(Clone, Default)]
struct CandidateRepo {
    /// The repository it redirects to, if not itself.
    moved_to: Option<Url>,
    is_private: bool,
}

impl CandidateRepo {
    fn new(repo: &Url, info: Option<&RepoInfo>) -> Self {
        Self {
            moved_to: info
                .map(|info| &info.repo)
                .filter(|to| !same_repo(to, repo))
                .cloned(),
            is_private: info.is_some_and(|info| info.is_private),
        }
    }
}

/// What the API says about a GitHub release asset.
enum GhAsset {
    Missing,
    Found(GhReleaseArtifactUrl),
    /// The API could not tell.
    Unknown,
}

/// The candidates of one target, checked with as few requests as possible.
pub(super) struct Probe {
    shared: Shared,
    candidates: Vec<Candidate>,
}

/// What checking a candidate needs, shared by the checks running at once.
struct Shared {
    data: Arc<Data>,
    client: Client,
    gh_api_client: GhApiClient,
    repo: Option<CompactString>,
    subcrate: Option<CompactString>,
}

impl Probe {
    /// `data`'s repository info, if needed, must be got first, with
    /// [`Data::get_repo_info`]. Without it the crate's repository is
    /// treated as any other.
    pub(super) async fn new(
        data: Arc<Data>,
        client: Client,
        gh_api_client: GhApiClient,
        repo: Option<&str>,
        subcrate: Option<&str>,
        candidates: Vec<Candidate>,
    ) -> Self {
        let mut probe = Self {
            shared: Shared {
                data,
                client,
                gh_api_client,
                repo: repo.map(CompactString::from),
                subcrate: subcrate.map(CompactString::from),
            },
            candidates,
        };
        probe.find_repos().await;
        probe
    }

    /// Look up the repository of each candidate whose release is known:
    ///
    /// - where it redirects to, if it was renamed or moved, so that the
    ///   candidates are checked there without a redirect for each request;
    /// - whether it is a private GitHub repository, whose assets can only be
    ///   checked and downloaded with the API.
    ///
    /// The crate's repository is known already. Another one is looked up
    /// once per run, see [`FetchContext`](crate::FetchContext).
    async fn find_repos(&mut self) {
        let Self { shared, candidates } = self;
        let crate_repo = shared.data.repo_info.get().and_then(Option::as_ref);

        // Normally one or two repositories.
        let mut repos = Vec::<(Url, CandidateRepo)>::new();
        for candidate in candidates.iter_mut() {
            let Some(release_tag) = &candidate.release_tag else {
                continue;
            };
            let repo = release_tag.repo.clone();
            let info = match repos.iter().find(|(r, _)| *r == repo) {
                Some((_, info)) => info.clone(),
                None => {
                    let info = match crate_repo {
                        Some(info) if same_repo(&info.repo, &repo) => {
                            CandidateRepo::new(&repo, Some(info))
                        }
                        _ => shared.other_repo_info(&repo).await,
                    };
                    repos.push((repo.clone(), info.clone()));
                    info
                }
            };
            if let Some(to) = &info.moved_to {
                candidate.move_to(&repo, to);
            }
            candidate.is_private = info.is_private;
        }
    }

    /// Check the candidates of the `pkg_url` and `pkg_fmt` a previous
    /// version was found with with GET, one at a time, before any other
    /// request. A hint that records what else the URL was rendered with has
    /// only one.
    ///
    /// The ones not found are dropped, so [`Probe::resolve`] doesn't check
    /// them again.
    ///
    /// More than two, one per archive suffix and `{ target-arch }` name of an
    /// older hint, are left to [`Probe::resolve`], which needs fewer requests
    /// for them.
    ///
    /// So is the hint of a private GitHub repository, whose assets can't be
    /// checked without the API: ls-refs normally leaves the candidates of one
    /// release, whose assets are then all asked with one API request.
    pub(super) async fn resolve_hint(
        &mut self,
        hint: &PkgUrlHint,
    ) -> Result<Option<Resolved>, FetchError> {
        let is_hinted = |candidate: &Candidate| {
            candidate.pkg_fmt == hint.pkg_fmt
                && *candidate.pkg_url == *hint.pkg_url
                && hint.rendered_with.as_ref().is_none_or(|rendered_with| {
                    candidate.archive_suffix == rendered_with.archive_suffix.as_deref()
                        && candidate.target_arch == rendered_with.target_arch.as_deref()
                })
        };
        if self
            .candidates
            .iter()
            .any(|candidate| candidate.is_private && is_hinted(candidate))
        {
            debug!("Leaving the hint of a private repository to ls-refs and the API");
            return Ok(None);
        }
        if self.candidates.iter().positions(is_hinted).nth(2).is_some() {
            debug!("Too many candidates for the hint to check one at a time");
            return Ok(None);
        }

        while let Some(i) = self.candidates.iter().position(is_hinted) {
            let candidate = self.candidates.swap_remove(i);
            let resolved = self.shared.check_with_get(candidate).await?;
            if resolved.is_some() {
                return Ok(resolved);
            }
        }
        Ok(None)
    }

    /// Check all the remaining candidates, returning the first one found:
    ///
    /// 1. Drop the candidates of the release tags that don't exist, see
    ///    [`Probe::drop_missing_tags`]. Before and after, the only candidate
    ///    of a public repository is checked with GET right away, see
    ///    [`Probe::check_only_candidate`].
    /// 2. For a public GitHub repository, check the candidates github.com's
    ///    asset lists have, and drop the ones they don't, see
    ///    [`Probe::check_on_web`].
    /// 3. With a token, get the assets of the GitHub releases github.com
    ///    could not tell about in one GraphQL query.
    /// 4. Check the rest at once.
    pub(super) async fn resolve(mut self) -> Result<Option<Resolved>, FetchError> {
        if let Some(resolved) = self.check_only_candidate().await? {
            return Ok(resolved);
        }
        self.drop_missing_tags().await;
        if self.candidates.is_empty() {
            debug!("No release tag of any candidate exists");
            return Ok(None);
        }
        if let Some(resolved) = self.check_only_candidate().await? {
            return Ok(resolved);
        }

        if let Some(resolved) = self.check_on_web().await? {
            return Ok(Some(resolved));
        }
        if self.candidates.is_empty() {
            debug!("github.com has none of the candidates");
            return Ok(None);
        }

        self.prefetch_gh_releases().await;

        let shared = Arc::new(self.shared);
        let resolver = FuturesResolver::default();
        resolver.extend(self.candidates.into_iter().map(|candidate| {
            let shared = Arc::clone(&shared);
            async move { shared.check(candidate).await }
        }));
        Ok(resolver.resolve().await)
    }

    /// Check the only candidate left of a public repository with GET, e.g.
    /// of a `pkg-url` pointing at cargo-quickinstall: one request, as many
    /// as ls-refs or github.com's asset list take to tell whether it may
    /// exist, and the GET is the download if found.
    ///
    /// `None` if there are more, or its tag is known not to exist already,
    /// so that [`Probe::drop_missing_tags`] drops it without a request.
    async fn check_only_candidate(&mut self) -> Result<Option<Option<Resolved>>, FetchError> {
        let Self { shared, candidates } = self;
        let [candidate] = &candidates[..] else {
            return Ok(None);
        };
        let is_tag_missing = match &candidate.release_tag {
            Some(ReleaseTag { repo, tag }) => {
                shared.data.context.repo_tags(repo).lock().await.exists(tag) == Some(false)
            }
            None => false,
        };
        if candidate.is_private || is_tag_missing {
            return Ok(None);
        }

        let candidate = candidates.pop().expect("one candidate");
        shared.check_with_get(candidate).await.map(Some)
    }

    /// Find out which release tags of the candidates exist, with one request
    /// to each repository's git server, and drop the candidates of the other
    /// ones. If a git server can't tell, its candidates are kept.
    ///
    /// A repository whose candidates are all of one release isn't asked:
    /// its asset list or the API tells that the release is missing with one
    /// request just as well, e.g. for a `pkg-url` pointing at
    /// cargo-quickinstall, which has a release per crate version.
    ///
    /// A private repository on github.com is asked with the token.
    ///
    /// The answers are kept in the [`FetchContext`](crate::FetchContext):
    /// only tags no target or crate asked about yet are asked, normally none
    /// after the first target.
    async fn drop_missing_tags(&mut self) {
        let Self { shared, candidates } = self;

        let repos = candidates
            .iter()
            .filter_map(|candidate| {
                let ReleaseTag { repo, tag } = candidate.release_tag.as_ref()?;
                Some((repo, (tag.as_str(), candidate.is_private)))
            })
            .into_grouping_map()
            .collect::<BTreeSet<_>>();

        let mut missing = HashSet::new();
        for (repo, tags) in repos {
            let is_private = tags.iter().any(|(_, is_private)| *is_private);
            let tags = || tags.iter().map(|(tag, _)| *tag);

            let known = shared.data.context.repo_tags(repo);
            let mut known = known.lock().await;
            let unasked = known.unasked(tags());
            if !unasked.is_empty() && tags().dedup().nth(1).is_some() {
                let listed = match GhRepo::try_extract_from_url(repo) {
                    Some(gh_repo) if is_private => {
                        shared
                            .gh_api_client
                            .list_private_repo_tags(&gh_repo, unasked)
                            .await
                    }
                    _ => git_refs::list_tags(&shared.client, repo, unasked, None).await,
                };
                known.add(
                    listed
                        .inspect_err(|err| {
                            debug!("Failed to list the release tags of {repo}: {err}")
                        })
                        .ok(),
                );
            }

            missing.extend(
                tags()
                    .filter(|tag| known.exists(tag) == Some(false))
                    .map(|tag| (repo.clone(), CompactString::from(tag))),
            );
        }

        if !missing.is_empty() {
            candidates.retain(|candidate| {
                candidate
                    .release_tag
                    .as_ref()
                    .is_none_or(|ReleaseTag { repo, tag }| {
                        !missing.contains(&(repo.clone(), tag.clone()))
                    })
            });
        }
    }

    /// Look up the candidates of each GitHub release on github.com, which
    /// costs no API quota. The ones it lists are checked with GET right away,
    /// returning the first one found; the ones it doesn't are dropped.
    ///
    /// Only for a public repository, github.com answers 404 for a private
    /// one. The list comes from a page meant for people, so a release's
    /// candidates are kept to check some other way if it lists none of them
    /// but some other assets.
    async fn check_on_web(&mut self) -> Result<Option<Resolved>, FetchError> {
        let mut unknown = Vec::new();

        while let Some(release) = self.candidates.iter().find_map(|candidate| {
            let artifact = candidate.gh_artifact.as_ref()?;
            (!candidate.is_private).then(|| artifact.release.clone())
        }) {
            let mut of_release: Vec<Candidate> = self
                .candidates
                .extract_if(.., |candidate| {
                    candidate
                        .gh_artifact
                        .as_ref()
                        .is_some_and(|artifact| artifact.release == release)
                })
                .collect();

            let names = of_release.iter().filter_map(|candidate| {
                Some(candidate.gh_artifact.as_ref()?.artifact_name.as_str())
            });
            let page = self.shared.data.context.release_assets_page(&release);
            let page = page
                .get_or_init(|| ReleaseAssetsPage::fetch(&self.shared.client, &release))
                .await;
            match page.lists(names) {
                // Nothing it could have left out.
                ReleaseAssets::NotFound | ReleaseAssets::NoAssets => (),
                // Normally only one is listed, the others are other formats.
                ReleaseAssets::Listed(listed) if !listed.is_empty() => {
                    let listed = of_release
                        .into_iter()
                        .enumerate()
                        .filter(|(i, _)| listed.contains(i));
                    for (_, candidate) in listed {
                        if let Some(resolved) = self.shared.check_with_get(candidate).await? {
                            return Ok(Some(resolved));
                        }
                    }
                }
                // It might not list every asset.
                ReleaseAssets::Listed(_) | ReleaseAssets::Unknown => {
                    unknown.append(&mut of_release)
                }
            }
        }

        self.candidates.append(&mut unknown);
        Ok(None)
    }

    /// With a token, get the assets of the GitHub releases of the candidates
    /// left, in one GraphQL query per repository, instead of one per
    /// release.
    async fn prefetch_gh_releases(&self) {
        let Shared {
            data,
            gh_api_client,
            ..
        } = &self.shared;
        if !gh_api_client.can_batch() {
            return;
        }

        let releases = self
            .candidates
            .iter()
            .filter_map(|candidate| candidate.gh_artifact.as_ref())
            .map(|artifact| (&artifact.release.repo, &artifact.release.tag))
            .into_grouping_map()
            .collect::<BTreeSet<_>>();

        // Normally all candidates are in the same repository.
        for (gh_repo, tags) in releases {
            let (tags, cells): (Vec<CompactString>, Vec<_>) = tags
                .into_iter()
                .map(|tag| {
                    let release = GhRelease {
                        repo: gh_repo.clone(),
                        tag: tag.clone(),
                    };
                    let cell = data.context.release_artifacts(&release);
                    (release.tag, cell)
                })
                .filter(|(_, cell)| !cell.initialized())
                .unzip();
            // A batch of one is the query `check` sends anyway.
            if tags.len() < 2 {
                continue;
            }

            match gh_api_client.get_releases_artifacts(gh_repo, &tags).await {
                Ok(releases) => {
                    for (cell, release) in cells.iter().zip(releases) {
                        // A failed one is asked again on its own by `check`.
                        if let Ok(artifacts) = release {
                            // Fails only if `check` got there first, then that
                            // answer is just as good.
                            let _ = cell.set(artifacts);
                        }
                    }
                }
                // The repository doesn't exist.
                Err(GhApiError::NotFound) => cells.iter().for_each(|cell| {
                    let _ = cell.set(None);
                }),
                Err(err) => {
                    debug!("Failed to get the assets of the releases of {gh_repo:?}: {err}")
                }
            }
        }
    }
}

impl Shared {
    /// [`CandidateRepo`] of `repo`, which is not the crate's repository.
    /// Kept in the [`FetchContext`](crate::FetchContext), so looked up once
    /// per run. If it can't be, its assets are checked as public ones where
    /// they are.
    async fn other_repo_info(&self, repo: &Url) -> CandidateRepo {
        let info = self.data.context.repo_info(repo.as_str());
        match RepoInfo::get(&info, Some(repo.as_str()), &self.gh_api_client).await {
            Ok(info) => CandidateRepo::new(repo, info),
            Err(err) => {
                debug!("Failed to look up {repo}: {err}");
                CandidateRepo::default()
            }
        }
    }

    /// Check `candidate` with the API if it is a GitHub release asset, with
    /// GET otherwise or if the API can't tell.
    async fn check(&self, candidate: Candidate) -> Result<Option<Resolved>, FetchError> {
        let Some(artifact) = &candidate.gh_artifact else {
            return self.check_with_get(candidate).await;
        };

        debug!("Checking for package with the API: '{}'", candidate.url);
        match self
            .check_gh_artifact(artifact, candidate.is_private)
            .await?
        {
            GhAsset::Found(artifact_url) => {
                Ok(Some(self.resolved(candidate, Some(artifact_url), None)))
            }
            GhAsset::Missing => Ok(None),
            GhAsset::Unknown => self.check_with_get(candidate).await,
        }
    }

    async fn check_with_get(&self, candidate: Candidate) -> Result<Option<Resolved>, FetchError> {
        debug!("Checking for package at: '{}'", candidate.url);
        Ok(get_if_exists(&self.client, &candidate.url)
            .await?
            .map(|response| self.resolved(candidate, None, Some(response))))
    }

    /// * `response` - The response to the GET that found `candidate`, to
    ///   download it from.
    fn resolved(
        &self,
        candidate: Candidate,
        gh_release_artifact_url: Option<GhReleaseArtifactUrl>,
        response: Option<GetResponse>,
    ) -> Resolved {
        Resolved {
            url: candidate.url,
            pkg_fmt: candidate.pkg_fmt,
            archive_suffix: candidate.archive_suffix.map(ToString::to_string),
            repo: self.repo.clone(),
            subcrate: self.subcrate.clone(),
            gh_release_artifact_url,
            is_repo_private: candidate.is_private,
            target_arch: candidate.target_arch,
            pkg_url: candidate.pkg_url,
            response,
            bin: None,
        }
    }

    /// Check a GitHub release asset with the API, which is asked once per
    /// release.
    ///
    /// * `is_private` - Whether its repository is private, see
    ///   [`Candidate::is_private`].
    async fn check_gh_artifact(
        &self,
        artifact: &GhReleaseArtifact,
        is_private: bool,
    ) -> Result<GhAsset, FetchError> {
        let cell = self.data.context.release_artifacts(&artifact.release);
        loop {
            let artifacts = cell
                .get_or_try_init(|| {
                    get_gh_release_artifacts(&self.gh_api_client, &artifact.release)
                })
                .await;
            match artifacts {
                Ok(artifacts) => {
                    return Ok(
                        match artifacts
                            .as_ref()
                            .and_then(|artifacts| artifacts.artifact_url(&artifact.artifact_name))
                        {
                            Some(artifact_url) => GhAsset::Found(artifact_url),
                            None => GhAsset::Missing,
                        },
                    );
                }

                // A public release asset can still be checked with GET.
                Err(GhApiError::RateLimit { .. } | GhApiError::Unauthorized) if !is_private => {
                    return Ok(GhAsset::Unknown);
                }
                Err(GhApiError::RateLimit { retry_after }) => {
                    match gh_api_rate_limit_wait(retry_after) {
                        Some(wait) => sleep(wait).await,
                        None => return Err(GhApiError::RateLimit { retry_after }.into()),
                    }
                }

                Err(err) => return Err(err.into()),
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{
        env,
        num::{NonZeroU16, NonZeroU64},
    };

    use binstalk_types::crate_info::RenderedWith;

    use super::*;
    use crate::gh_crate_meta::hosting::RepositoryHost;

    const BINSTALL: &str = "https://github.com/cargo-bins/cargo-binstall";
    const QUICKINSTALL: &str = "https://github.com/cargo-bins/cargo-quickinstall";
    const PRIVATE: &str = "https://github.com/cargo-bins/private-repo-for-testing";

    fn client() -> Client {
        Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
            NonZeroU16::new(300).unwrap(),
            NonZeroU64::new(1).unwrap(),
            [],
        )
        .unwrap()
    }

    const TEMPLATE: &str =
        "{ repo }/releases/download/{ version }/{ name }-{ target }{ archive-suffix }";
    const OTHER_TEMPLATE: &str =
        "{ repo }/releases/download/{ version }/{ name }{ archive-suffix }";

    /// The `.tgz` candidate for `{repo}/releases/download/{asset}`, rendered
    /// from `pkg_url`.
    fn new_candidate(repo: &str, asset: &str, pkg_url: &str) -> Candidate {
        new_candidate_with(repo, asset, pkg_url, PkgFmt::Tgz, Some(".tgz"), None)
    }

    fn new_candidate_with(
        repo: &str,
        asset: &str,
        pkg_url: &str,
        pkg_fmt: PkgFmt,
        archive_suffix: Option<&'static str>,
        target_arch: Option<&'static str>,
    ) -> Candidate {
        let url = Url::parse(&format!("{repo}/releases/download/{asset}")).unwrap();
        Candidate::new(url, pkg_fmt, archive_suffix, target_arch, pkg_url.into())
    }

    /// A hint of [`TEMPLATE`].
    fn hint(pkg_fmt: PkgFmt, rendered_with: Option<RenderedWith>) -> PkgUrlHint {
        PkgUrlHint {
            name: "cargo-binstall".into(),
            pkg_url: TEMPLATE.into(),
            pkg_fmt,
            rendered_with,
        }
    }

    async fn new_probe(
        gh_api_client: &GhApiClient,
        repo: &str,
        candidates: Vec<Candidate>,
    ) -> Probe {
        let data = Data::new(
            Default::default(),
            "test".into(),
            "0.0.0".into(),
            Some(repo.into()),
        );
        data.get_repo_info(gh_api_client).await.unwrap().unwrap();
        Probe::new(
            Arc::new(data),
            client(),
            gh_api_client.clone(),
            Some(repo),
            None,
            candidates,
        )
        .await
    }

    /// The asset of `resolved`, `{tag}/{name}`.
    fn asset(resolved: Option<Resolved>) -> Option<String> {
        let path = resolved?.url.path().to_owned();
        Some(path.split_once("/releases/download/").unwrap().1.to_owned())
    }

    /// How the candidates of a public repository were resolved.
    #[derive(Debug, PartialEq)]
    struct Probed {
        /// The asset found, `{tag}/{name}`.
        found: Option<String>,
        /// Candidates left once the missing tags were dropped.
        after_tags: usize,
        /// Candidates left once github.com was asked, `None` if it was
        /// found there.
        after_web: Option<usize>,
    }

    /// Resolve the candidates for `assets` of the public `repo`.
    async fn probe(repo: &str, assets: &[&str]) -> Probed {
        probe_with(&GhApiClient::new(client(), None), repo, assets).await
    }

    async fn probe_with(gh_api_client: &GhApiClient, repo: &str, assets: &[&str]) -> Probed {
        let candidates = assets
            .iter()
            .map(|asset| new_candidate(repo, asset, OTHER_TEMPLATE))
            .collect();
        let mut probe = new_probe(gh_api_client, repo, candidates).await;
        assert!(
            !probe
                .candidates
                .iter()
                .any(|candidate| candidate.is_private)
        );

        probe.drop_missing_tags().await;
        let after_tags = probe.candidates.len();

        if let Some(found) = probe.check_on_web().await.unwrap() {
            return Probed {
                found: asset(Some(found)),
                after_tags,
                after_web: None,
            };
        }
        let after_web = Some(probe.candidates.len());

        Probed {
            found: asset(probe.resolve().await.unwrap()),
            after_tags,
            after_web,
        }
    }

    /// `Probed` of an asset found on github.com.
    fn on_web(found: &str, after_tags: usize) -> Probed {
        Probed {
            found: Some(found.to_owned()),
            after_tags,
            after_web: None,
        }
    }

    /// `Probed` of nothing found.
    fn not_found(after_tags: usize, after_web: usize) -> Probed {
        Probed {
            found: None,
            after_tags,
            after_web: Some(after_web),
        }
    }

    #[test]
    fn test_candidate() {
        let tag = |candidate: &Candidate| {
            candidate
                .release_tag
                .as_ref()
                .map(|ReleaseTag { repo, tag }| (repo.to_string(), tag.to_string()))
        };
        let some = |repo: &str, tag: &str| Some((repo.to_owned(), tag.to_owned()));

        let candidate = new_candidate(BINSTALL, "v0.20.1/a.tgz", OTHER_TEMPLATE);
        assert_eq!(tag(&candidate), some(BINSTALL, "v0.20.1"));
        let artifact = candidate.gh_artifact.unwrap();
        assert_eq!(artifact.release.tag, "v0.20.1");
        assert_eq!(artifact.artifact_name, "a.tgz");

        // A tag with `+`, as in the URL rendered from the template.
        let candidate = new_candidate(
            QUICKINSTALL,
            "a-1.0.0+b-2.0.0/a-1.0.0+b-2.0.0-x.tar.gz",
            OTHER_TEMPLATE,
        );
        assert_eq!(tag(&candidate), some(QUICKINSTALL, "a-1.0.0+b-2.0.0"));
        let artifact = candidate.gh_artifact.unwrap();
        assert_eq!(artifact.release.tag, "a-1.0.0+b-2.0.0");
        assert_eq!(artifact.artifact_name, "a-1.0.0+b-2.0.0-x.tar.gz");

        // Not on GitHub, but the tag is still known.
        let repo = "https://gitlab.com/a/b";
        let url = Url::parse(&format!("{repo}/-/releases/v1.0.0/downloads/a.tgz")).unwrap();
        let candidate = Candidate::new(url, PkgFmt::Tgz, None, None, OTHER_TEMPLATE.into());
        assert_eq!(tag(&candidate), some(repo, "v1.0.0"));
        assert!(candidate.gh_artifact.is_none());

        // In another repository than the crate's.
        let candidate = |url: &str| {
            Candidate::new(
                Url::parse(url).unwrap(),
                PkgFmt::Tgz,
                None,
                None,
                OTHER_TEMPLATE.into(),
            )
        };
        let quickinstall = candidate(&format!("{QUICKINSTALL}/releases/download/a-1.0.0/a.tgz"));
        assert_eq!(tag(&quickinstall), some(QUICKINSTALL, "a-1.0.0"));
        let codeberg = candidate("https://codeberg.org/a/b/releases/download/v1.0.0/a.tgz");
        assert_eq!(tag(&codeberg), some("https://codeberg.org/a/b", "v1.0.0"));
        // On any host, such as a self-hosted Gitea or Forgejo.
        let repo = "https://git.example.com/a/b";
        let self_hosted = candidate(&format!("{repo}/releases/download/v1.0.0/a.tgz"));
        assert_eq!(tag(&self_hosted), some(repo, "v1.0.0"));
    }

    #[tokio::test]
    async fn test_probe_cargo_binstall() {
        // We would never release anything older than v0.20.1
        let musl_tgz = "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz";
        // The missing tag is dropped, then the asset github.com lists is
        // found without checking the one it doesn't.
        assert_eq!(
            probe(
                BINSTALL,
                &[
                    "v0.18.2/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                    "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.zip",
                    musl_tgz,
                ],
            )
            .await,
            on_web(musl_tgz, 2)
        );

        // github.com lists none of them: it might not list every asset, so
        // they are kept and checked some other way.
        assert_eq!(
            probe(
                BINSTALL,
                &[
                    "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.zip",
                    "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
                ],
            )
            .await,
            not_found(2, 2)
        );

        // A tag without a release: nothing to download.
        assert_eq!(
            probe(
                BINSTALL,
                &["detect-targets-v0.1.0/detect-targets-x86_64-unknown-linux-musl.tgz"],
            )
            .await,
            not_found(1, 0)
        );

        // No tag exists: github.com is not asked.
        assert_eq!(
            probe(
                BINSTALL,
                &[
                    "v0.0.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                    "0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                ],
            )
            .await,
            not_found(0, 0)
        );

        // The latest release, with `.full` builds and signatures. The
        // signature is not listed under the name of the archive.
        let found = probe(
            BINSTALL,
            &[
                "v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.tgz.sig",
                "v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.full.tgz",
                "v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.zip",
            ],
        )
        .await;
        assert_eq!(found.after_web, None);
        assert!(found.found.is_some_and(|found| {
            found.starts_with("v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.")
        }));

        // Assets of two releases: the one github.com lists is found, though
        // the other release lists none of its candidates, which are kept.
        assert_eq!(
            probe(
                BINSTALL,
                &[
                    "v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
                    "v1.28.0/cargo-binstall-x86_64-unknown-none.tgz",
                    musl_tgz,
                ],
            )
            .await,
            on_web(musl_tgz, 3)
        );

        // The owner and repository written in another case, as crates
        // sometimes do.
        let repo = "https://github.com/Cargo-Bins/Cargo-Binstall";
        assert_eq!(probe(repo, &[musl_tgz]).await, on_web(musl_tgz, 1));
    }

    /// The next target, tried when one has nothing, reuses what the first
    /// one asked the git server and github.com, and so do other crates of
    /// the same repository.
    #[tokio::test]
    async fn test_probe_next_target() {
        let gh_api_client = GhApiClient::new(client(), None);
        let context = Arc::<crate::FetchContext>::default();
        let data = Data::new(
            context.clone(),
            "test".into(),
            "0.0.0".into(),
            Some(BINSTALL.into()),
        );
        data.get_repo_info(&gh_api_client).await.unwrap();
        let data = Arc::new(data);
        let probe = |data: &Arc<Data>, assets: &[&str]| {
            let data = data.clone();
            let gh_api_client = gh_api_client.clone();
            let candidates = assets
                .iter()
                .map(|asset| new_candidate(BINSTALL, asset, OTHER_TEMPLATE))
                .collect();
            async move {
                let mut probe = Probe::new(
                    data,
                    client(),
                    gh_api_client,
                    Some(BINSTALL),
                    None,
                    candidates,
                )
                .await;
                probe.drop_missing_tags().await;
                probe.check_on_web().await.unwrap()
            }
        };

        // A target v0.20.1 has no build for. Two candidates of the release,
        // so github.com's asset list is used.
        let riscv = [
            "v0.20.1/cargo-binstall-riscv64gc-unknown-linux-gnu.tgz",
            "v0.20.1/cargo-binstall-riscv64gc-unknown-linux-gnu.zip",
            "v0.18.2/cargo-binstall-riscv64gc-unknown-linux-gnu.tgz",
        ];
        assert!(probe(&data, &riscv).await.is_none());
        let musl_tgz = "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz";
        let musl = [
            musl_tgz,
            "v0.18.2/cargo-binstall-x86_64-unknown-linux-musl.tgz",
        ];
        assert_eq!(asset(probe(&data, &musl).await).as_deref(), Some(musl_tgz));

        // One ls-refs answer, one page.
        let repo = &data.repo_info.get().unwrap().as_ref().unwrap().repo;
        let repo_tags = context.repo_tags(repo);
        let tags = repo_tags.lock().await;
        assert_eq!(tags.exists("v0.20.1"), Some(true));
        assert_eq!(tags.exists("v0.18.2"), Some(false));
        assert!(tags.unasked(["v0.20.1", "v0.18.2"]).is_empty());
        drop(tags);
        assert_eq!(context.release_assets_pages_len(), 1);

        // The crate looked up by its binary's name shares all of it: only
        // the tags not asked yet are asked.
        let binary_data = Arc::new(data.with_name("binary".into()));
        assert!(binary_data.repo_info.get().is_some());
        let found = probe(
            &binary_data,
            &[
                musl_tgz,
                "binary-v0.20.1/binary-x86_64-unknown-linux-musl.tgz",
            ],
        )
        .await;
        assert_eq!(asset(found).as_deref(), Some(musl_tgz));
        let tags = repo_tags.lock().await;
        assert_eq!(tags.exists("binary-v0.20.1"), Some(false));
        assert_eq!(tags.exists("v0.20.1"), Some(true));
        drop(tags);
        assert_eq!(context.release_assets_pages_len(), 1);

        // Another crate, whose `repository` is a subdirectory of the same
        // repository, shares the tags and pages too.
        let other = Data::new(
            context.clone(),
            "other".into(),
            "0.0.0".into(),
            Some(format!("{BINSTALL}/tree/main/crates/bin")),
        );
        let info = other.get_repo_info(&gh_api_client).await.unwrap().unwrap();
        assert_eq!(info.subcrate.as_deref(), Some("bin"));
        assert!(Arc::ptr_eq(&context.repo_tags(&info.repo), &repo_tags));
        let found = probe(&Arc::new(other), &musl).await;
        assert_eq!(asset(found).as_deref(), Some(musl_tgz));
        assert!(
            repo_tags
                .lock()
                .await
                .unasked(["v0.20.1", "v0.18.2"])
                .is_empty()
        );
        assert_eq!(context.release_assets_pages_len(), 1);
    }

    /// A public repository needs no API even with a token.
    ///
    /// Only meaningful with a token that isn't valid, which the API would
    /// reject, so in CI: locally the proxy signs every request in.
    #[tokio::test]
    async fn test_probe_public_repo_with_token() {
        let gh_api_client = GhApiClient::new(
            client(),
            Some(
                "ghp_000000000000000000000000000000000000"
                    .to_owned()
                    .into_boxed_str()
                    .into(),
            ),
        );
        let musl_tgz = "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz";
        assert_eq!(
            probe_with(
                &gh_api_client,
                BINSTALL,
                &[
                    "v0.18.2/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                    musl_tgz,
                ],
            )
            .await,
            on_web(musl_tgz, 1)
        );
        // The API would have rejected it.
        assert!(gh_api_client.has_gh_token());
    }

    #[tokio::test]
    async fn test_probe_cargo_quickinstall() {
        // A tag with `+`, and a target it has no build for.
        let found = "cargo-c-0.10.26+cargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz";
        assert_eq!(
            probe(
                QUICKINSTALL,
                &[
                    "cargo-c-0.10.26+cargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-aarch64-unknown-linux-gnu.tar.gz",
                    found,
                    "cargo-c-v0.10.26+cargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz",
                ],
            )
            .await,
            on_web(found, 2)
        );

        // A tag that is a prefix of others, `ripgrep-14.1.10` and so on.
        let found = "ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz";
        assert_eq!(
            probe(
                QUICKINSTALL,
                &[
                    "ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tgz",
                    found,
                    "ripgrep-14.1.10/ripgrep-14.1.10-x86_64-unknown-linux-gnu.tar.gz",
                ],
            )
            .await,
            on_web(found, 2)
        );

        // A target it has no build for, so github.com lists other assets.
        assert_eq!(
            probe(
                QUICKINSTALL,
                &[
                    "ripgrep-14.1.1/ripgrep-14.1.1-riscv64gc-unknown-none-elf.tar.gz",
                    "ripgrep-14.1.1/ripgrep-14.1.1-riscv64gc-unknown-none-elf.tgz",
                ],
            )
            .await,
            not_found(2, 2)
        );
    }

    /// The only candidate, as of a `pkg-url` pointing at cargo-quickinstall,
    /// is checked with GET, without ls-refs or github.com's asset list: one
    /// request, which is the download if found.
    #[tokio::test]
    async fn test_resolve_only_candidate() {
        let gh_api_client = GhApiClient::new(client(), None);
        const GNU: &str = "ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz";
        let resolve = |assets: &'static [&'static str]| {
            let gh_api_client = gh_api_client.clone();
            async move {
                // The crate's own repository is another one.
                let candidates = assets
                    .iter()
                    .map(|asset| {
                        let url = Url::parse(&format!("{QUICKINSTALL}/releases/download/{asset}"));
                        Candidate::new(url.unwrap(), PkgFmt::Tgz, None, None, OTHER_TEMPLATE.into())
                    })
                    .collect();
                let probe = new_probe(&gh_api_client, BINSTALL, candidates).await;
                let context = Arc::clone(&probe.shared.data.context);
                (probe.resolve().await.unwrap(), context)
            }
        };
        let quickinstall = Url::parse(QUICKINSTALL).unwrap();

        let (found, context) = resolve(&[GNU]).await;
        let found = found.unwrap();
        assert!(found.response.is_some());
        assert_eq!(asset(Some(found)).as_deref(), Some(GNU));
        assert_eq!(context.release_assets_pages_len(), 0);
        let tags = context.repo_tags(&quickinstall);
        assert_eq!(tags.lock().await.exists("ripgrep-14.1.1"), None);

        // Not found.
        let (found, context) =
            resolve(&["ripgrep-14.1.1/ripgrep-14.1.1-riscv64gc-unknown-none-elf.tar.gz"]).await;
        assert!(found.is_none());
        assert_eq!(context.release_assets_pages_len(), 0);

        // The only one ls-refs leaves, asked in cargo-quickinstall, not the
        // crate's repository.
        let (found, context) = resolve(&[
            "ripgrep-14.1.0/ripgrep-14.1.0-x86_64-unknown-linux-gnu.tar.gz",
            GNU,
        ])
        .await;
        assert_eq!(asset(found).as_deref(), Some(GNU));
        assert_eq!(context.release_assets_pages_len(), 0);
        let tags = context.repo_tags(&quickinstall);
        let tags = tags.lock().await;
        assert_eq!(tags.exists("ripgrep-14.1.0"), Some(false));
        assert_eq!(tags.exists("ripgrep-14.1.1"), Some(true));
        drop(tags);

        // Two of one release left: not asked with ls-refs, github.com's
        // asset list is used.
        let (found, context) = resolve(&[
            "ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tgz",
            GNU,
        ])
        .await;
        assert_eq!(asset(found).as_deref(), Some(GNU));
        assert_eq!(context.release_assets_pages_len(), 1);
        let tags = context.repo_tags(&quickinstall);
        assert_eq!(tags.lock().await.exists("ripgrep-14.1.1"), None);
    }

    /// Only the assets of a private repository are checked as private ones,
    /// not those of a `pkg-url` pointing at another, public one.
    #[tokio::test]
    async fn test_candidate_is_private() {
        let data = Data::new(
            Default::default(),
            "test".into(),
            "0.0.0".into(),
            Some(PRIVATE.into()),
        );
        // What `get_repo_info` finds with a token.
        data.repo_info
            .set(Some(RepoInfo {
                repo: Url::parse(PRIVATE).unwrap(),
                repository_host: RepositoryHost::GitHub,
                subcrate: None,
                is_private: true,
            }))
            .unwrap();
        // cargo-quickinstall is looked up, and public.
        let probe = Probe::new(
            Arc::new(data),
            client(),
            GhApiClient::new(client(), None),
            Some(PRIVATE),
            None,
            vec![
                new_candidate(PRIVATE, "0.12.0/a.tgz", OTHER_TEMPLATE),
                // The owner and repository written in another case.
                new_candidate(
                    "https://github.com/Cargo-Bins/Private-Repo-For-Testing",
                    "0.12.0/a.tgz",
                    OTHER_TEMPLATE,
                ),
                new_candidate(QUICKINSTALL, "a-0.12.0/a.tgz", OTHER_TEMPLATE),
            ],
        )
        .await;
        let is_private: Vec<_> = probe.candidates.iter().map(|c| c.is_private).collect();
        assert_eq!(is_private, [true, true, false]);
    }

    #[test]
    fn test_candidate_move_to() {
        let mut candidate = new_candidate(
            // Written in another case than the repository's.
            "https://github.com/Alsuren/Cargo-Quickinstall",
            "a-1.0.0+b/a-1.0.0+b-x.tar.gz",
            OTHER_TEMPLATE,
        );
        let from = candidate.release_tag.clone().unwrap().repo;
        let to = Url::parse(QUICKINSTALL).unwrap();
        candidate.move_to(&from, &to);
        assert_eq!(
            candidate.url.as_str(),
            format!("{QUICKINSTALL}/releases/download/a-1.0.0+b/a-1.0.0+b-x.tar.gz")
        );
        let artifact = candidate.gh_artifact.unwrap();
        assert_eq!(
            artifact.release.repo,
            GhRepo::try_extract_from_url(&to).unwrap()
        );
        assert_eq!(artifact.release.tag, "a-1.0.0+b");
        assert_eq!(artifact.artifact_name, "a-1.0.0+b-x.tar.gz");
        let tag = candidate.release_tag.unwrap();
        assert_eq!(
            (tag.repo.as_str(), tag.tag.as_str()),
            (QUICKINSTALL, "a-1.0.0+b")
        );

        // Not on GitHub, and moved to another host.
        let url = "https://gitlab.com/a/b/-/releases/v1.0.0/downloads/a.tgz";
        let mut candidate = Candidate::new(
            Url::parse(url).unwrap(),
            PkgFmt::Tgz,
            None,
            None,
            OTHER_TEMPLATE.into(),
        );
        let from = candidate.release_tag.clone().unwrap().repo;
        let to = Url::parse("https://git.example.com/c/d").unwrap();
        candidate.move_to(&from, &to);
        assert_eq!(
            candidate.url.as_str(),
            "https://git.example.com/c/d/-/releases/v1.0.0/downloads/a.tgz"
        );
        let tag = candidate.release_tag.unwrap();
        assert_eq!(tag.repo, to);
        assert_eq!(tag.tag, "v1.0.0");
    }

    /// A candidate in a repository that was transferred is checked where it
    /// is now, without a redirect for each request.
    ///
    /// github.com answers 403 for the pages of repositories that aren't
    /// attached to Claude Code sessions, so this only passes in CI.
    #[tokio::test]
    async fn test_candidate_redirected() {
        let gh_api_client = GhApiClient::new(client(), None);
        let probe = new_probe(
            &gh_api_client,
            BINSTALL,
            vec![new_candidate(
                "https://github.com/alsuren/cargo-quickinstall",
                "a-1.0.0/a.tgz",
                OTHER_TEMPLATE,
            )],
        )
        .await;
        assert_eq!(
            probe.candidates[0].url.as_str(),
            format!("{QUICKINSTALL}/releases/download/a-1.0.0/a.tgz")
        );
        assert!(!probe.candidates[0].is_private);
    }

    /// ls-refs against a host that isn't a git server fails once per run, and
    /// the candidates are kept for the checks after it.
    #[tokio::test]
    async fn test_drop_missing_tags_not_git() {
        let gh_api_client = GhApiClient::new(client(), None);
        // The repository of a release asset URL on a download server.
        const NOT_GIT: &str = "https://static.crates.io/a/b";
        let mut probe = new_probe(
            &gh_api_client,
            BINSTALL,
            vec![
                new_candidate(NOT_GIT, "v1.0.0/a.tgz", OTHER_TEMPLATE),
                new_candidate(NOT_GIT, "1.0.0/a.tgz", OTHER_TEMPLATE),
            ],
        )
        .await;
        probe.drop_missing_tags().await;
        assert_eq!(probe.candidates.len(), 2);

        let tags = probe
            .shared
            .data
            .context
            .repo_tags(&Url::parse(NOT_GIT).unwrap());
        let tags = tags.lock().await;
        assert_eq!(tags.exists("v1.0.0"), None);
        // Not asked again.
        assert!(tags.unasked(["v1.0.0", "2.0.0"]).is_empty());
    }

    #[tokio::test]
    async fn test_resolve_hint() {
        let gh_api_client = GhApiClient::new(client(), None);
        let musl_tgz = "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz";

        // A hint written before `rendered_with` was recorded.
        let candidates = vec![
            new_candidate(
                BINSTALL,
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.zip",
                TEMPLATE,
            ),
            new_candidate(BINSTALL, musl_tgz, OTHER_TEMPLATE),
            new_candidate(BINSTALL, musl_tgz, TEMPLATE),
        ];
        let mut probe = new_probe(&gh_api_client, BINSTALL, candidates).await;

        let found = probe.resolve_hint(&hint(PkgFmt::Tgz, None)).await.unwrap();
        assert_eq!(asset(found).as_deref(), Some(musl_tgz));
        // The one not found is dropped, the one of another template is
        // left for `resolve`.
        assert_eq!(probe.candidates.len(), 1);
        assert_eq!(&*probe.candidates[0].pkg_url, OTHER_TEMPLATE);

        // Another pkg-fmt.
        assert!(
            probe
                .resolve_hint(&hint(PkgFmt::Zip, None))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(probe.candidates.len(), 1);

        // More than two are left to `resolve`, without a request.
        let tgz = |asset, archive_suffix, target_arch| {
            new_candidate_with(
                BINSTALL,
                asset,
                TEMPLATE,
                PkgFmt::Tgz,
                archive_suffix,
                target_arch,
            )
        };
        let candidates = || {
            vec![
                tgz(
                    "v0.20.1/cargo-binstall-amd64-unknown-linux-musl.tgz",
                    Some(".tgz"),
                    Some("amd64"),
                ),
                tgz(
                    "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
                    Some(".tar.gz"),
                    None,
                ),
                tgz(musl_tgz, Some(".tgz"), None),
            ]
        };
        let mut probe = new_probe(&gh_api_client, BINSTALL, candidates()).await;
        assert!(
            probe
                .resolve_hint(&hint(PkgFmt::Tgz, None))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(probe.candidates.len(), 3);
        assert_eq!(
            asset(probe.resolve().await.unwrap()).as_deref(),
            Some(musl_tgz)
        );

        // A hint that records the archive suffix and the architecture's name
        // matches only one.
        let exact = hint(
            PkgFmt::Tgz,
            Some(RenderedWith {
                archive_suffix: Some(".tgz".into()),
                target_arch: None,
            }),
        );
        let mut probe = new_probe(&gh_api_client, BINSTALL, candidates()).await;
        let found = probe.resolve_hint(&exact).await.unwrap();
        assert_eq!(asset(found).as_deref(), Some(musl_tgz));
        assert_eq!(probe.candidates.len(), 2);

        // The alias it was found with.
        let amd64 = hint(
            PkgFmt::Tgz,
            Some(RenderedWith {
                archive_suffix: Some(".tgz".into()),
                target_arch: Some("amd64".into()),
            }),
        );
        let mut probe = new_probe(&gh_api_client, BINSTALL, candidates()).await;
        assert!(probe.resolve_hint(&amd64).await.unwrap().is_none());
        // Only the one it named was checked and dropped.
        assert_eq!(probe.candidates.len(), 2);
        assert!(
            probe
                .candidates
                .iter()
                .all(|candidate| candidate.target_arch.is_none())
        );
    }

    /// Needs `CI_UNIT_TEST_GITHUB_TOKEN` with access to the private repository.
    #[tokio::test]
    async fn test_probe_private_repo() {
        let Some(auth_token) = env::var("CI_UNIT_TEST_GITHUB_TOKEN")
            .ok()
            .filter(|token| !token.is_empty())
        else {
            return;
        };
        let gh_api_client = GhApiClient::new(client(), Some(auth_token.into_boxed_str().into()));

        let assets = [
            "v0.12.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
            "0.12.0/cargo-binstall-x86_64-unknown-linux-musl.zip",
            "0.12.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
        ];
        let candidates = assets
            .iter()
            .map(|asset| new_candidate(PRIVATE, asset, OTHER_TEMPLATE))
            .collect();
        let mut probe = new_probe(&gh_api_client, PRIVATE, candidates).await;
        assert!(
            probe
                .candidates
                .iter()
                .all(|candidate| candidate.is_private)
        );

        // The git server was asked with the token.
        probe.drop_missing_tags().await;
        assert_eq!(probe.candidates.len(), 2);

        let found = probe.resolve().await.unwrap().unwrap();
        // Downloaded with the API.
        assert!(found.gh_release_artifact_url.is_some());
        assert_eq!(
            asset(Some(found)).as_deref(),
            Some("0.12.0/cargo-binstall-x86_64-unknown-linux-musl.tgz")
        );

        // The hint of a private repository is left to `resolve`, without a
        // request, which finds it with ls-refs and one API request for the
        // one release left.
        let tgz = |asset, archive_suffix, target_arch| {
            new_candidate_with(
                PRIVATE,
                asset,
                TEMPLATE,
                PkgFmt::Tgz,
                archive_suffix,
                target_arch,
            )
        };
        let candidates = vec![
            tgz(
                "v0.12.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                Some(".tgz"),
                None,
            ),
            tgz(
                "0.12.0/cargo-binstall-amd64-unknown-linux-musl.tgz",
                Some(".tgz"),
                Some("amd64"),
            ),
            tgz(
                "0.12.0/cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
                Some(".tar.gz"),
                None,
            ),
            tgz(assets[2], Some(".tgz"), None),
        ];
        let mut probe = new_probe(&gh_api_client, PRIVATE, candidates).await;
        let exact = hint(
            PkgFmt::Tgz,
            Some(RenderedWith {
                archive_suffix: Some(".tgz".into()),
                target_arch: None,
            }),
        );
        assert!(probe.resolve_hint(&exact).await.unwrap().is_none());
        assert_eq!(probe.candidates.len(), 4);
        assert_eq!(probe.shared.data.context.release_artifacts_len(), 0);

        let context = Arc::clone(&probe.shared.data.context);
        let found = probe.resolve().await.unwrap().unwrap();
        assert!(found.gh_release_artifact_url.is_some());
        assert_eq!(asset(Some(found)).as_deref(), Some(assets[2]));
        assert_eq!(context.release_artifacts_len(), 1);

        // The private repository is not the crate's, which is public: found
        // out with the token, and its tags are asked with it too.
        let candidates = assets
            .iter()
            .map(|asset| {
                Candidate::new(
                    Url::parse(&format!("{PRIVATE}/releases/download/{asset}")).unwrap(),
                    PkgFmt::Tgz,
                    None,
                    None,
                    OTHER_TEMPLATE.into(),
                )
            })
            .collect();
        let mut probe = new_probe(&gh_api_client, BINSTALL, candidates).await;
        assert!(
            probe
                .candidates
                .iter()
                .all(|candidate| candidate.is_private)
        );
        probe.drop_missing_tags().await;
        assert_eq!(probe.candidates.len(), 2);
        let found = probe.resolve().await.unwrap().unwrap();
        assert!(found.gh_release_artifact_url.is_some());
        assert!(found.is_repo_private);
        assert_eq!(asset(Some(found)).as_deref(), Some(assets[2]));
    }
}
