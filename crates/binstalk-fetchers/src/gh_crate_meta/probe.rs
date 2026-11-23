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

use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};

use binstalk_git_repo_api::{
    gh_api_client::{
        GhApiError, GhRelease, GhReleaseArtifact, GhReleaseArtifactUrl, GhRepo, ReleaseAssets,
        ReleaseAssetsPage,
    },
    git_refs,
};
use compact_str::CompactString;
use itertools::Itertools;
use tokio::time::sleep;
use url::Url;

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

/// What the API says about a GitHub release asset.
enum GhAsset {
    Missing,
    Found(GhReleaseArtifactUrl),
    /// The API could not tell.
    Unknown,
}

/// The candidates of one target, checked with as few requests as possible.
pub(super) struct Probe {
    data: Arc<Data>,
    shared: Shared,
    candidates: Vec<Candidate>,
}

/// What checking a candidate needs, shared by the checks running at once.
struct Shared {
    client: Client,
    gh_api_client: GhApiClient,
    repo: Option<CompactString>,
    subcrate: Option<CompactString>,
    is_repo_private: bool,
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
        Self {
            data,
            shared: Shared {
                client,
                gh_api_client,
                repo: repo.map(CompactString::from),
                subcrate: subcrate.map(CompactString::from),
                is_repo_private,
            },
            candidates,
        }
    }

    /// Check the candidates of the default `pkg_url` and `pkg_fmt` a previous
    /// version was found with, one at a time, before any other request: with
    /// GET, or with the API for a private GitHub repository, whose assets
    /// can't be downloaded without it.
    ///
    /// The ones not found are dropped, so [`Probe::resolve`] doesn't check
    /// them again.
    ///
    /// More than two, one per archive suffix and `{ target-arch }` name,
    /// are left to [`Probe::resolve`], which needs fewer requests for them.
    pub(super) async fn resolve_hint(
        &mut self,
        pkg_url: &str,
        pkg_fmt: PkgFmt,
    ) -> Result<Option<Resolved>, FetchError> {
        let is_hinted = |candidate: &Candidate| {
            candidate.pkg_fmt == pkg_fmt && candidate.default_pkg_url.as_deref() == Some(pkg_url)
        };
        if self.candidates.iter().positions(is_hinted).nth(2).is_some() {
            debug!("Too many candidates for the hint to check one at a time");
            return Ok(None);
        }

        while let Some(i) = self.candidates.iter().position(is_hinted) {
            let candidate = self.candidates.swap_remove(i);
            let resolved = if self.shared.is_repo_private {
                self.shared.check(candidate).await?
            } else {
                self.shared.check_with_get(candidate).await?
            };
            if resolved.is_some() {
                return Ok(resolved);
            }
        }
        Ok(None)
    }

    /// Check all the remaining candidates, returning the first one found:
    ///
    /// 1. Drop the candidates of the release tags that don't exist, see
    ///    [`Probe::drop_missing_tags`].
    /// 2. For a public GitHub repository, check the candidates github.com's
    ///    asset lists have, and drop the ones they don't, see
    ///    [`Probe::check_on_web`].
    /// 3. With a token, get the assets of the GitHub releases github.com
    ///    could not tell about in one GraphQL query.
    /// 4. Check the rest at once.
    pub(super) async fn resolve(mut self) -> Result<Option<Resolved>, FetchError> {
        self.drop_missing_tags().await;
        if self.candidates.is_empty() {
            debug!("No release tag of any candidate exists");
            return Ok(None);
        }

        if !self.shared.is_repo_private {
            if let Some(resolved) = self.check_on_web().await? {
                return Ok(Some(resolved));
            }
            if self.candidates.is_empty() {
                debug!("github.com has none of the candidates");
                return Ok(None);
            }
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

    /// Find out which release tags of the candidates exist, with one request
    /// to the repository's git server, and drop the candidates of the other
    /// ones. If the git server can't tell, every candidate is kept.
    ///
    /// A private repository on github.com is asked with the token.
    ///
    /// The answer is kept in [`Data`]: if this target has nothing, the next
    /// one is tried with the same tags.
    async fn drop_missing_tags(&mut self) {
        let Self {
            data,
            shared,
            candidates,
        } = self;

        let tags = data
            .repo_tags
            .get_or_init(|| async {
                let tags: HashSet<CompactString> = candidates
                    .iter()
                    .filter_map(|candidate| candidate.tag.clone())
                    .collect();
                if tags.is_empty() {
                    return None;
                }

                // Set by `get_repo_info`, which `find` calls first.
                let RepoInfo {
                    repo, is_private, ..
                } = data.repo_info.get()?.as_ref()?;

                let tags = match GhRepo::try_extract_from_url(repo) {
                    Some(gh_repo) if *is_private => {
                        shared
                            .gh_api_client
                            .list_private_repo_tags(&gh_repo, tags)
                            .await
                    }
                    _ => git_refs::list_tags(&shared.client, repo, tags, None).await,
                };
                tags.inspect_err(|err| debug!("Failed to list the release tags of {repo}: {err}"))
                    .ok()
            })
            .await;

        if let Some(tags) = tags {
            candidates.retain(|candidate| {
                candidate.tag.as_deref().and_then(|tag| tags.exists(tag)) != Some(false)
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

        while let Some(release) = self
            .candidates
            .iter()
            .find_map(|candidate| Some(candidate.gh_artifact.as_ref()?.release.clone()))
        {
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
            match self.release_assets_on_web(release, names).await {
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

    /// Which of the asset `names` of `release` github.com lists, with the
    /// page kept in [`Data`] for the other targets.
    async fn release_assets_on_web<'a>(
        &self,
        release: GhRelease,
        names: impl IntoIterator<Item = &'a str>,
    ) -> ReleaseAssets {
        let pages = &self.data.release_assets;
        if let Some((_, page)) = pages.lock().unwrap().iter().find(|(r, _)| *r == release) {
            return page.lists(names);
        }

        let page = ReleaseAssetsPage::fetch(&self.shared.client, &release).await;
        let assets = page.lists(names);
        pages.lock().unwrap().push((release, page));
        assets
    }

    /// With a token, get the assets of the GitHub releases of the candidates
    /// left, in one GraphQL query per repository, instead of one per
    /// release.
    async fn prefetch_gh_releases(&self) {
        let gh_api_client = &self.shared.gh_api_client;
        if !gh_api_client.has_gh_token() {
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
            gh_api_client
                .prefetch_release_artifacts(gh_repo, tags)
                .await;
        }
    }
}

impl Shared {
    /// Check `candidate` with the API if it is a GitHub release asset, with
    /// GET otherwise or if the API can't tell.
    async fn check(&self, candidate: Candidate) -> Result<Option<Resolved>, FetchError> {
        let Some(artifact) = &candidate.gh_artifact else {
            return self.check_with_get(candidate).await;
        };

        debug!("Checking for package with the API: '{}'", candidate.url);
        match self.check_gh_artifact(artifact).await? {
            GhAsset::Found(artifact_url) => Ok(Some(self.resolved(candidate, Some(artifact_url)))),
            GhAsset::Missing => Ok(None),
            GhAsset::Unknown => self.check_with_get(candidate).await,
        }
    }

    async fn check_with_get(&self, candidate: Candidate) -> Result<Option<Resolved>, FetchError> {
        debug!("Checking for package at: '{}'", candidate.url);
        Ok(does_url_exist(&self.client, &candidate.url)
            .await?
            .then(|| self.resolved(candidate, None)))
    }

    fn resolved(
        &self,
        candidate: Candidate,
        gh_release_artifact_url: Option<GhReleaseArtifactUrl>,
    ) -> Resolved {
        Resolved {
            url: candidate.url,
            pkg_fmt: candidate.pkg_fmt,
            archive_suffix: candidate.archive_suffix.map(ToString::to_string),
            repo: self.repo.clone(),
            subcrate: self.subcrate.clone(),
            gh_release_artifact_url,
            is_repo_private: self.is_repo_private,
            target_arch: candidate.target_arch,
            default_pkg_url: candidate.default_pkg_url,
        }
    }

    /// Check a GitHub release asset with the API.
    async fn check_gh_artifact(&self, artifact: &GhReleaseArtifact) -> Result<GhAsset, FetchError> {
        loop {
            match get_gh_release_artifact_url(self.gh_api_client.clone(), artifact.clone()).await {
                Ok(Some(artifact_url)) => return Ok(GhAsset::Found(artifact_url)),
                Ok(None) => return Ok(GhAsset::Missing),

                // A public release asset can still be checked with GET.
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
}

#[cfg(test)]
mod test {
    use std::{
        env,
        num::{NonZeroU16, NonZeroU64},
    };

    use super::*;

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

    /// The candidate for `{repo}/releases/download/{asset}`, rendered from
    /// the default `pkg_url` if any.
    fn new_candidate(repo: &str, asset: &str, default_pkg_url: Option<&str>) -> Candidate {
        let url = Url::parse(&format!("{repo}/releases/download/{asset}")).unwrap();
        let default_pkg_url = default_pkg_url.map(CompactString::from);
        Candidate::new(
            url,
            PkgFmt::Tgz,
            Some(".tgz"),
            None,
            default_pkg_url,
            Some(repo),
        )
    }

    async fn new_probe(
        gh_api_client: &GhApiClient,
        repo: &str,
        candidates: Vec<Candidate>,
    ) -> Probe {
        let data = Data::new("test".into(), "0.0.0".into(), Some(repo.into()));
        let info = data.get_repo_info(gh_api_client).await.unwrap().unwrap();
        let is_repo_private = info.is_private;
        Probe::new(
            Arc::new(data),
            client(),
            gh_api_client.clone(),
            Some(repo),
            None,
            is_repo_private,
            candidates,
        )
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
            .map(|asset| new_candidate(repo, asset, None))
            .collect();
        let mut probe = new_probe(gh_api_client, repo, candidates).await;
        assert!(!probe.shared.is_repo_private);

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
        let candidate = new_candidate(BINSTALL, "v0.20.1/a.tgz", None);
        assert_eq!(candidate.tag.as_deref(), Some("v0.20.1"));
        let artifact = candidate.gh_artifact.unwrap();
        assert_eq!(artifact.release.tag, "v0.20.1");
        assert_eq!(artifact.artifact_name, "a.tgz");

        // A tag with `+`, as in the URL rendered from the template.
        let candidate = new_candidate(
            QUICKINSTALL,
            "a-1.0.0+b-2.0.0/a-1.0.0+b-2.0.0-x.tar.gz",
            None,
        );
        assert_eq!(candidate.tag.as_deref(), Some("a-1.0.0+b-2.0.0"));
        let artifact = candidate.gh_artifact.unwrap();
        assert_eq!(artifact.release.tag, "a-1.0.0+b-2.0.0");
        assert_eq!(artifact.artifact_name, "a-1.0.0+b-2.0.0-x.tar.gz");

        // Not on GitHub, but the tag is still known.
        let repo = "https://gitlab.com/a/b";
        let url = Url::parse(&format!("{repo}/-/releases/v1.0.0/downloads/a.tgz")).unwrap();
        let candidate = Candidate::new(url, PkgFmt::Tgz, None, None, None, Some(repo));
        assert_eq!(candidate.tag.as_deref(), Some("v1.0.0"));
        assert!(candidate.gh_artifact.is_none());
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
    /// one asked the git server and github.com.
    #[tokio::test]
    async fn test_probe_next_target() {
        let gh_api_client = GhApiClient::new(client(), None);
        let data = Data::new("test".into(), "0.0.0".into(), Some(BINSTALL.into()));
        data.get_repo_info(&gh_api_client).await.unwrap();
        let data = Arc::new(data);
        let probe = |assets: &[&str]| {
            let mut probe = Probe::new(
                data.clone(),
                client(),
                gh_api_client.clone(),
                Some(BINSTALL),
                None,
                false,
                assets
                    .iter()
                    .map(|asset| new_candidate(BINSTALL, asset, None))
                    .collect(),
            );
            async move {
                probe.drop_missing_tags().await;
                probe.check_on_web().await.unwrap()
            }
        };

        // A target v0.20.1 has no build for.
        assert!(
            probe(&[
                "v0.20.1/cargo-binstall-riscv64gc-unknown-linux-gnu.tgz",
                "v0.18.2/cargo-binstall-riscv64gc-unknown-linux-gnu.tgz",
            ])
            .await
            .is_none()
        );
        let musl_tgz = "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz";
        let found = probe(&[
            musl_tgz,
            "v0.18.2/cargo-binstall-x86_64-unknown-linux-musl.tgz",
        ])
        .await;
        assert_eq!(asset(found).as_deref(), Some(musl_tgz));

        // One ls-refs answer, one page.
        assert!(data.repo_tags.get().unwrap().is_some());
        assert_eq!(data.release_assets.lock().unwrap().len(), 1);
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
                &["ripgrep-14.1.1/ripgrep-14.1.1-riscv64gc-unknown-none-elf.tar.gz"],
            )
            .await,
            not_found(1, 1)
        );
    }

    #[tokio::test]
    async fn test_resolve_hint() {
        let gh_api_client = GhApiClient::new(client(), None);
        let hint = "{ repo }/releases/download/{ version }/{ name }-{ target }.{ archive-format }";
        let candidates = vec![
            new_candidate(
                BINSTALL,
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.zip",
                Some(hint),
            ),
            new_candidate(
                BINSTALL,
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                None,
            ),
            new_candidate(
                BINSTALL,
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz",
                Some(hint),
            ),
        ];
        let mut probe = new_probe(&gh_api_client, BINSTALL, candidates).await;

        let found = probe.resolve_hint(hint, PkgFmt::Tgz).await.unwrap();
        assert_eq!(
            asset(found).as_deref(),
            Some("v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz")
        );
        // The one not found is dropped, the one of another template is
        // left for `resolve`.
        assert_eq!(probe.candidates.len(), 1);
        assert_eq!(probe.candidates[0].default_pkg_url, None);

        // Another pkg-fmt.
        assert!(
            probe
                .resolve_hint(hint, PkgFmt::Zip)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(probe.candidates.len(), 1);

        // More than two are left to `resolve`, without a request.
        let candidates = [
            "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz",
            "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
            "v0.20.1/cargo-binstall-amd64-unknown-linux-musl.tgz",
        ]
        .iter()
        .map(|asset| new_candidate(BINSTALL, asset, Some(hint)))
        .collect();
        let mut probe = new_probe(&gh_api_client, BINSTALL, candidates).await;
        assert!(
            probe
                .resolve_hint(hint, PkgFmt::Tgz)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(probe.candidates.len(), 3);
        assert_eq!(
            asset(probe.resolve().await.unwrap()).as_deref(),
            Some("v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tgz")
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
            .map(|asset| new_candidate(PRIVATE, asset, None))
            .collect();
        let mut probe = new_probe(&gh_api_client, PRIVATE, candidates).await;
        assert!(probe.shared.is_repo_private);

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

        // The hint of a private repository is checked with the API.
        let hint = "{ repo }/releases/download/{ version }/{ name }-{ target }.{ archive-format }";
        let candidates = vec![new_candidate(PRIVATE, assets[2], Some(hint))];
        let mut probe = new_probe(&gh_api_client, PRIVATE, candidates).await;
        let found = probe
            .resolve_hint(hint, PkgFmt::Tgz)
            .await
            .unwrap()
            .unwrap();
        assert!(found.gh_release_artifact_url.is_some());
    }
}
