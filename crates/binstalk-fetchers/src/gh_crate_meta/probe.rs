//! Checking which of the URLs a package may be at exists, with as few
//! requests as possible.
//!
//! [`Candidate`]s are rendered once per target. What their releases share is
//! found out once, in [`Probe::new`], before any of them is checked:
//!
//! 1. Which release tags exist, with one request to the repository's git
//!    server, see [`git_refs`]. The candidates of a missing tag are dropped.
//! 2. For GitHub, the asset names of the remaining releases: from github.com
//!    for a public repository (see [`Probe::check_on_web`]), which costs no
//!    API quota, or with one GraphQL query for a private one (see
//!    [`Probe::prefetch_gh_releases`]).
//!
//! Only then is each candidate checked, see [`Probe::check`].

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use binstalk_git_repo_api::{
    gh_api_client::{
        GhApiError, GhRelease, GhReleaseArtifact, GhReleaseArtifactUrl, ReleaseAssets,
    },
    git_refs::{self, RepoTags},
};
use compact_str::CompactString;
use itertools::Itertools;
use tokio::time::sleep;
use url::Url;

use super::Resolved;
use crate::{
    Data, FetchError, common::*, futures_resolver::FuturesResolver, gh_api_rate_limit_wait,
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
    /// The default `pkg-url` it was rendered from, `None` if the crate
    /// specified `pkg-url`.
    pub(super) default_pkg_url: Option<CompactString>,
    /// The tag of the release it is an asset of, if known.
    tag: Option<CompactString>,
    /// The GitHub release asset it is.
    gh_artifact: Option<GhReleaseArtifact>,
}

impl Candidate {
    pub(super) fn new(
        url: Url,
        pkg_fmt: PkgFmt,
        archive_suffix: Option<&'static str>,
        target_arch: Option<&'static str>,
        default_pkg_url: Option<CompactString>,
        repo: Option<&str>,
    ) -> Self {
        Self {
            tag: repo.and_then(|repo| git_refs::release_tag(repo, &url)),
            gh_artifact: GhReleaseArtifact::try_extract_from_url(&url),
            url,
            pkg_fmt,
            archive_suffix,
            target_arch,
            default_pkg_url,
        }
    }
}

/// What a GitHub release asset is found to be.
enum GhAsset {
    Missing,
    /// It exists, with its API URL if the API was asked.
    Found(Option<GhReleaseArtifactUrl>),
    /// Neither the API nor github.com could tell.
    Unknown,
}

/// What the candidates of one target share.
pub(super) struct Probe {
    client: Client,
    gh_api_client: GhApiClient,
    repo: Option<String>,
    subcrate: Option<String>,
    is_repo_private: bool,
    /// Which release tags exist, if the repository's git server could tell.
    tags: Option<RepoTags>,
    /// The candidate asset names of each GitHub release.
    gh_candidates: HashMap<GhRelease, HashSet<CompactString>>,
}

impl Probe {
    /// Find out which release tags of `candidates` exist and drop the
    /// candidates of the other ones, before any API call.
    pub(super) async fn new(
        client: Client,
        gh_api_client: GhApiClient,
        data: &Data,
        repo: Option<&str>,
        subcrate: Option<&str>,
        is_repo_private: bool,
        candidates: &mut Vec<Candidate>,
    ) -> Self {
        // One request to the repository's git server, shared by all targets.
        // A private repository needs authentication.
        let tags = match repo {
            Some(repo) if !is_repo_private => data
                .get_repo_tags(
                    &client,
                    repo,
                    candidates.iter().filter_map(|c| c.tag.as_deref()),
                )
                .await
                .cloned(),
            _ => None,
        };

        let mut probe = Self {
            client,
            gh_api_client,
            repo: repo.map(ToString::to_string),
            subcrate: subcrate.map(ToString::to_string),
            is_repo_private,
            tags,
            gh_candidates: HashMap::new(),
        };

        candidates.retain(|candidate| probe.tag_exists(candidate) != Some(false));

        probe.gh_candidates = candidates
            .iter()
            .filter_map(|candidate| candidate.gh_artifact.clone())
            .map(|artifact| (artifact.release, artifact.artifact_name))
            .into_grouping_map()
            .collect();

        probe
    }

    /// Whether the release tag of `candidate` exists, if known.
    fn tag_exists(&self, candidate: &Candidate) -> Option<bool> {
        self.tags.as_ref()?.exists(candidate.tag.as_deref()?)
    }

    /// For a private repository, get the assets of all the candidates' GitHub
    /// releases in one GraphQL query per repository, instead of one per
    /// release.
    ///
    /// A public repository's are looked up on github.com instead, see
    /// [`Probe::check_on_web`].
    pub(super) async fn prefetch_gh_releases(&self) {
        if !self.is_repo_private || !self.gh_api_client.has_gh_token() {
            return;
        }

        let releases = self
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

    /// Check all of `candidates` at once, returning the first one found.
    pub(super) async fn resolve(
        self: &Arc<Self>,
        candidates: impl IntoIterator<Item = Candidate>,
    ) -> Option<Resolved> {
        let resolver = FuturesResolver::default();
        resolver.extend(
            candidates
                .into_iter()
                .map(|candidate| Arc::clone(self).check(candidate)),
        );
        resolver.resolve().await
    }

    async fn check(self: Arc<Self>, candidate: Candidate) -> Result<Option<Resolved>, FetchError> {
        debug!("Checking for package at: '{}'", candidate.url);

        let gh_asset = match &candidate.gh_artifact {
            Some(artifact) => self.check_gh_artifact(artifact, &candidate).await?,
            None => GhAsset::Unknown,
        };
        let gh_release_artifact_url = match gh_asset {
            GhAsset::Found(url) => url,
            GhAsset::Missing => return Ok(None),
            GhAsset::Unknown => {
                if !Box::pin(self.client.remote_gettable(candidate.url.clone())).await? {
                    return Ok(None);
                }
                None
            }
        };

        Ok(Some(Resolved {
            url: candidate.url,
            pkg_fmt: candidate.pkg_fmt,
            archive_suffix: candidate.archive_suffix.map(ToString::to_string),
            repo: self.repo.clone(),
            subcrate: self.subcrate.clone(),
            gh_release_artifact_url,
            is_repo_private: self.is_repo_private,
            target_arch: candidate.target_arch,
            default_pkg_url: candidate.default_pkg_url,
        }))
    }

    /// Check a GitHub release asset on github.com for a public repository,
    /// with the API if github.com can't tell or the repository is private.
    async fn check_gh_artifact(
        &self,
        artifact: &GhReleaseArtifact,
        candidate: &Candidate,
    ) -> Result<GhAsset, FetchError> {
        // github.com costs no API quota, which is 60 requests per hour
        // without a token, and the API is shared with everything else using
        // the token.
        if !self.is_repo_private {
            match self.check_on_web(artifact, candidate).await? {
                GhAsset::Unknown => (),
                found => return Ok(found),
            }
        }

        loop {
            match get_gh_release_artifact_url(self.gh_api_client.clone(), artifact.clone()).await {
                Ok(Some(artifact_url)) => return Ok(GhAsset::Found(Some(artifact_url))),
                Ok(None) => return Ok(GhAsset::Missing),

                // A public release asset can be checked without the API.
                Err(GhApiError::RateLimit { .. } | GhApiError::Unauthorized)
                    if !self.is_repo_private =>
                {
                    return self.check_on_web(artifact, candidate).await;
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

    /// Check a release asset of a public repository on github.com, without
    /// the API.
    ///
    /// The release checks are cached per release, so this costs at most two
    /// requests per release however many candidates are checked, plus one for
    /// each candidate found.
    async fn check_on_web(
        &self,
        artifact: &GhReleaseArtifact,
        candidate: &Candidate,
    ) -> Result<GhAsset, FetchError> {
        // If the git server could not tell, HEAD the release page first:
        // github.com answers a missing release with a 404 page about as large
        // as an asset list.
        if self.tag_exists(candidate).is_none()
            && !self.gh_api_client.has_release_page(&artifact.release).await
        {
            return Ok(GhAsset::Missing);
        }

        let assets = match self
            .gh_api_client
            .release_assets_on_web(&artifact.release)
            .await
        {
            ReleaseAssets::NotFound => return Ok(GhAsset::Missing),
            ReleaseAssets::Found(assets) => assets,
            ReleaseAssets::Unknown => return Ok(GhAsset::Unknown),
        };

        if assets.contains(&artifact.artifact_name) {
            // The list is parsed from a page meant for people: make sure a
            // listed asset can be downloaded before picking it.
            let found = Box::pin(self.client.remote_gettable(candidate.url.clone())).await?;
            return Ok(if found {
                GhAsset::Found(None)
            } else {
                GhAsset::Missing
            });
        }

        // The page might not list every asset. Only trust that this one is
        // missing if the page lists no asset at all, so there is nothing it
        // could have left out, or another candidate of the release; otherwise
        // check some other way.
        let lists_a_candidate = self
            .gh_candidates
            .get(&artifact.release)
            .is_some_and(|names| names.iter().any(|name| assets.contains(name)));
        Ok(if assets.is_empty() || lists_a_candidate {
            GhAsset::Missing
        } else {
            GhAsset::Unknown
        })
    }
}
