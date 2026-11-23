//! Checking which of the URLs a package may be at exists, with as few
//! requests as possible.
//!
//! [`Candidate`]s are rendered once per target. The ones of the `pkg-url` a
//! previous version was found with are checked first. Then what the releases
//! of the others share is found out once, before any of them is checked:
//!
//! 1. Which release tags exist, with one request to the repository's git
//!    server, see [`Probe::drop_missing_tags`]. The candidates of a missing
//!    tag are dropped.
//! 2. For GitHub, the asset names of the remaining releases: from github.com
//!    for a public repository (see [`Shared::check_on_web`]), which costs no
//!    API quota, or with one GraphQL query for a private one (see
//!    [`Probe::prefetch_gh_releases`]).
//!
//! Only then is each candidate checked, see [`Shared::check`].

use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

use binstalk_git_repo_api::{
    gh_api_client::{
        GhApiError, GhRelease, GhReleaseArtifact, GhReleaseArtifactUrl, ReleaseAssets,
    },
    git_refs,
};
use compact_str::CompactString;
use itertools::Itertools;
use tokio::{sync::OnceCell, time::sleep};
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

/// The candidates of one GitHub release.
struct GhReleaseCandidates {
    /// Their asset names, sorted.
    names: Vec<CompactString>,
    /// What the release's asset list on github.com says about them.
    on_web: OnceCell<OnWeb>,
}

/// What the asset list of a release on github.com says about its candidates.
enum OnWeb {
    Missing,
    /// The indices of the [`GhReleaseCandidates::names`] that are listed, at
    /// least one. Normally only one is, the others are other formats.
    Listed(Vec<usize>),
    /// github.com could not tell, or it lists none of them: it might not
    /// list every asset.
    Unknown,
}

/// The candidates of one target, checked with as few requests as possible.
pub(super) struct Probe {
    shared: Arc<Shared>,
    candidates: Vec<Candidate>,
}

/// What checking the candidates of one target needs, shared by the checks
/// running at once.
struct Shared {
    /// Which release tags exist is in there, once [`Probe::drop_missing_tags`]
    /// asked the repository's git server.
    data: Arc<Data>,
    client: Client,
    gh_api_client: GhApiClient,
    repo: Option<CompactString>,
    subcrate: Option<CompactString>,
    is_repo_private: bool,
    /// The candidates of each GitHub release.
    gh_releases: HashMap<GhRelease, GhReleaseCandidates>,
}

impl Probe {
    pub(super) fn new(
        data: Arc<Data>,
        client: Client,
        gh_api_client: GhApiClient,
        repo: Option<&str>,
        subcrate: Option<&str>,
        is_repo_private: bool,
        candidates: Vec<Candidate>,
    ) -> Self {
        let gh_releases = candidates
            .iter()
            .filter_map(|candidate| candidate.gh_artifact.clone())
            .map(|artifact| (artifact.release, artifact.artifact_name))
            .into_grouping_map()
            .collect::<Vec<_>>()
            .into_iter()
            .map(|(release, mut names)| {
                names.sort_unstable();
                names.dedup();
                let on_web = OnceCell::new();
                (release, GhReleaseCandidates { names, on_web })
            })
            .collect();

        Self {
            shared: Arc::new(Shared {
                data,
                client,
                gh_api_client,
                repo: repo.map(CompactString::from),
                subcrate: subcrate.map(CompactString::from),
                is_repo_private,
                gh_releases,
            }),
            candidates,
        }
    }

    /// Check the candidates `pred` is true for at once, returning the first
    /// one found. They are not checked again by [`Probe::resolve`].
    pub(super) fn resolve_where(
        &mut self,
        pred: impl FnMut(&mut Candidate) -> bool,
    ) -> impl Future<Output = Option<Resolved>> {
        self.shared
            .resolve(self.candidates.extract_if(.., pred).collect::<Vec<_>>())
    }

    /// Check all the remaining candidates at once, returning the first one
    /// found.
    pub(super) fn resolve(self) -> impl Future<Output = Option<Resolved>> {
        self.shared.resolve(self.candidates)
    }

    /// Find out which release tags of the candidates exist, with one request
    /// to the repository's git server shared by all targets, and drop the
    /// candidates of the other ones. Returns whether any candidate is left.
    ///
    /// A private repository on github.com is asked with the token. If the git
    /// server can't tell, every candidate is kept and checked with the API or
    /// GET as before.
    pub(super) async fn drop_missing_tags(&mut self) -> bool {
        let shared = &*self.shared;
        let tags = shared
            .data
            .get_repo_tags(
                &shared.client,
                &shared.gh_api_client,
                self.candidates.iter().filter_map(|c| c.tag.as_deref()),
            )
            .await;

        if tags.is_some() {
            self.candidates
                .retain(|candidate| shared.tag_exists(candidate.tag.as_deref()) != Some(false));
        }
        !self.candidates.is_empty()
    }

    /// For a private repository, get the assets of all the candidates' GitHub
    /// releases in one GraphQL query per repository, instead of one per
    /// release.
    ///
    /// A public repository's are looked up on github.com instead, see
    /// [`Shared::check_on_web`].
    pub(super) async fn prefetch_gh_releases(&self) {
        let shared = &*self.shared;
        if !shared.is_repo_private || !shared.gh_api_client.has_gh_token() {
            return;
        }

        let releases = self
            .candidates
            .iter()
            .filter_map(|candidate| candidate.gh_artifact.as_ref())
            .map(|artifact| (&artifact.release.repo, artifact.release.tag.clone()))
            .into_grouping_map()
            .collect::<BTreeSet<_>>();

        // Normally all candidates are in the same repository.
        for (gh_repo, tags) in releases {
            shared
                .gh_api_client
                .prefetch_release_artifacts(gh_repo, tags)
                .await;
        }
    }
}

impl Shared {
    /// Whether the release tag `tag` exists, if known.
    fn tag_exists(&self, tag: Option<&str>) -> Option<bool> {
        self.data.repo_tags.get()?.as_ref()?.exists(tag?)
    }

    fn resolve(
        self: &Arc<Self>,
        candidates: Vec<Candidate>,
    ) -> impl Future<Output = Option<Resolved>> + use<> {
        let resolver = FuturesResolver::default();
        resolver.extend(
            candidates
                .into_iter()
                .map(|candidate| Arc::clone(self).check(candidate)),
        );
        resolver.resolve()
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
                if !does_url_exist(&self.client, &candidate.url).await? {
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

                // github.com was already asked: a public release asset can
                // still be checked with GET.
                Err(GhApiError::RateLimit { .. } | GhApiError::Unauthorized)
                    if !self.is_repo_private =>
                {
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

    /// Check a release asset of a public repository on github.com, without
    /// the API.
    ///
    /// What the asset list says is worked out once per release, so this
    /// costs at most two requests per release however many candidates are
    /// checked, plus one for each candidate found.
    async fn check_on_web(
        &self,
        artifact: &GhReleaseArtifact,
        candidate: &Candidate,
    ) -> Result<GhAsset, FetchError> {
        let Some(release) = self.gh_releases.get(&artifact.release) else {
            return Ok(GhAsset::Unknown);
        };
        let on_web = release
            .on_web
            .get_or_init(|| self.read_asset_list(&artifact.release, &release.names))
            .await;

        let listed = match on_web {
            OnWeb::Missing => return Ok(GhAsset::Missing),
            OnWeb::Unknown => return Ok(GhAsset::Unknown),
            OnWeb::Listed(listed) => release
                .names
                .binary_search(&artifact.artifact_name)
                .is_ok_and(|i| listed.contains(&i)),
        };
        if !listed {
            // The list has another candidate of the release, so it is the
            // list of the assets of the same build.
            return Ok(GhAsset::Missing);
        }

        // The list is parsed from a page meant for people: make sure a
        // listed asset can be downloaded before picking it.
        Ok(if does_url_exist(&self.client, &candidate.url).await? {
            GhAsset::Found(None)
        } else {
            GhAsset::Missing
        })
    }

    /// What the asset list of `release` on github.com says about the
    /// candidates `names`.
    async fn read_asset_list(&self, release: &GhRelease, names: &[CompactString]) -> OnWeb {
        // If the git server could not tell, HEAD the release page first:
        // github.com answers a missing release with a 404 page about as large
        // as an asset list.
        if self.tag_exists(Some(&release.tag)).is_none()
            && !self.gh_api_client.has_release_page(release).await
        {
            return OnWeb::Missing;
        }

        let names = names.iter().map(CompactString::as_str);
        match self
            .gh_api_client
            .release_assets_on_web(release, names)
            .await
        {
            // Nothing it could have left out.
            ReleaseAssets::NotFound | ReleaseAssets::NoAssets => OnWeb::Missing,
            // It might not list every asset.
            ReleaseAssets::Listed(listed) if listed.is_empty() => OnWeb::Unknown,
            ReleaseAssets::Listed(listed) => OnWeb::Listed(listed),
            ReleaseAssets::Unknown => OnWeb::Unknown,
        }
    }
}

#[cfg(test)]
mod test {
    use std::{
        env,
        num::{NonZeroU16, NonZeroU64},
    };

    use super::*;

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

    fn candidates(repo: &str, assets: &[&str]) -> Vec<Candidate> {
        assets
            .iter()
            .map(|asset| {
                let url = Url::parse(&format!("{repo}/releases/download/{asset}")).unwrap();
                Candidate::new(url, PkgFmt::Tgz, Some(".tgz"), None, None, Some(repo))
            })
            .collect()
    }

    /// Resolve the candidates for `assets` the way `GhCrateMeta` does,
    /// returning the asset found and how many candidates were left to check
    /// once the missing tags were dropped.
    async fn probe(
        gh_api_client: GhApiClient,
        repo: &str,
        is_repo_private: bool,
        assets: &[&str],
    ) -> (Option<String>, usize) {
        let data = Data::new("test".into(), "0.0.0".into(), Some(repo.into()));
        data.get_repo_info(&gh_api_client).await.unwrap().unwrap();
        let mut probe = Probe::new(
            Arc::new(data),
            client(),
            gh_api_client,
            Some(repo),
            None,
            is_repo_private,
            candidates(repo, assets),
        );

        probe.drop_missing_tags().await;
        let left = probe.candidates.len();
        probe.prefetch_gh_releases().await;
        let found = probe.resolve().await.map(|resolved| {
            let path = resolved.url.path().to_owned();
            path.split_once("/releases/download/").unwrap().1.to_owned()
        });

        (found, left)
    }

    #[test]
    fn test_candidate() {
        let repo = "https://github.com/cargo-bins/cargo-binstall";
        let [candidate] = <[_; 1]>::try_from(candidates(repo, &["v0.20.1/a.tgz"])).unwrap();
        assert_eq!(candidate.tag.as_deref(), Some("v0.20.1"));
        let artifact = candidate.gh_artifact.unwrap();
        assert_eq!(artifact.release.tag, "v0.20.1");
        assert_eq!(artifact.artifact_name, "a.tgz");

        // Not on GitHub, but the tag is still known.
        let repo = "https://gitlab.com/a/b";
        let url = Url::parse(&format!("{repo}/-/releases/v1.0.0/downloads/a.tgz")).unwrap();
        let candidate = Candidate::new(url, PkgFmt::Tgz, None, None, None, Some(repo));
        assert_eq!(candidate.tag.as_deref(), Some("v1.0.0"));
        assert!(candidate.gh_artifact.is_none());
    }

    #[tokio::test]
    async fn test_probe_public_repo() {
        let repo = "https://github.com/cargo-bins/cargo-binstall";
        let gh_api_client = GhApiClient::new(client(), None);

        let (found, left) = probe(
            gh_api_client.clone(),
            repo,
            false,
            &[
                // We would never release anything older than v0.20.1
                "v0.18.2/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.zip",
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz",
            ],
        )
        .await;
        assert_eq!(
            found.as_deref(),
            Some("v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz")
        );
        // The candidate of the missing tag is dropped before any check.
        assert_eq!(left, 2);

        // Listed on github.com by another candidate of the release.
        let (found, left) = probe(
            gh_api_client,
            repo,
            false,
            &[
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.zip",
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
            ],
        )
        .await;
        assert_eq!(found, None);
        assert_eq!(left, 2);
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

        let repo = "https://github.com/cargo-bins/private-repo-for-testing";
        let gh_api_client = GhApiClient::new(client(), Some(auth_token.into_boxed_str().into()));

        let (found, left) = probe(
            gh_api_client,
            repo,
            true,
            &[
                "v0.12.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                "0.12.0/cargo-binstall-x86_64-unknown-linux-musl.zip",
                "0.12.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
            ],
        )
        .await;
        assert_eq!(
            found.as_deref(),
            Some("0.12.0/cargo-binstall-x86_64-unknown-linux-musl.tgz")
        );
        // The git server was asked with the token.
        assert_eq!(left, 2);
    }
}
