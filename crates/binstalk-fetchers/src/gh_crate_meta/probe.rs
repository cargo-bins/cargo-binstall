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

use std::{collections::BTreeSet, sync::Arc};

use binstalk_git_repo_api::{
    gh_api_client::{
        GhApiError, GhRelease, GhReleaseArtifact, GhReleaseArtifactUrl, ReleaseAssets,
    },
    git_refs,
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
    /// Whether to check it with GET only, without the API: github.com lists
    /// it (see [`Probe::check_on_web`]), or it is a hint for a public
    /// repository.
    get_only: bool,
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
            get_only: false,
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
    pub(super) async fn resolve_hint(
        &mut self,
        pkg_url: &str,
        pkg_fmt: PkgFmt,
    ) -> Result<Option<Resolved>, FetchError> {
        while let Some(i) = self.candidates.iter().position(|candidate| {
            candidate.pkg_fmt == pkg_fmt && candidate.default_pkg_url.as_deref() == Some(pkg_url)
        }) {
            let mut candidate = self.candidates.swap_remove(i);
            candidate.get_only = !self.shared.is_repo_private;
            if let Some(resolved) = self.shared.check(candidate).await? {
                return Ok(Some(resolved));
            }
        }
        Ok(None)
    }

    /// Check all the remaining candidates, returning the first one found:
    ///
    /// 1. Drop the candidates of the release tags that don't exist, see
    ///    [`Probe::drop_missing_tags`].
    /// 2. For a public GitHub repository, drop the candidates github.com's
    ///    asset lists don't have, see [`Probe::check_on_web`].
    /// 3. With a token, get the assets of the GitHub releases github.com
    ///    could not tell about in one GraphQL query.
    /// 4. Check the rest at once.
    pub(super) async fn resolve(mut self) -> Option<Resolved> {
        let tags_known = self.drop_missing_tags().await;
        if !self.shared.is_repo_private {
            self.check_on_web(tags_known).await;
        }
        if self.candidates.is_empty() {
            debug!("No release has any of the candidates");
            return None;
        }
        self.prefetch_gh_releases().await;

        let shared = Arc::new(self.shared);
        let resolver = FuturesResolver::default();
        resolver.extend(self.candidates.into_iter().map(|candidate| {
            let shared = Arc::clone(&shared);
            async move { shared.check(candidate).await }
        }));
        resolver.resolve().await
    }

    /// Find out which release tags of the candidates exist, with one request
    /// to the repository's git server shared by all targets, and drop the
    /// candidates of the other ones. Returns whether the git server could
    /// tell.
    ///
    /// A private repository on github.com is asked with the token.
    async fn drop_missing_tags(&mut self) -> bool {
        let Some(tags) = self
            .data
            .get_repo_tags(
                &self.shared.client,
                &self.shared.gh_api_client,
                self.candidates.iter().filter_map(|c| c.tag.as_deref()),
            )
            .await
        else {
            return false;
        };

        self.candidates.retain(|candidate| {
            candidate.tag.as_deref().and_then(|tag| tags.exists(tag)) != Some(false)
        });
        true
    }

    /// Look up the candidates of each GitHub release on github.com, which
    /// costs no API quota: drop the ones it doesn't list, and mark the ones
    /// it does to only be confirmed with GET.
    ///
    /// Only for a public repository, github.com answers 404 for a private
    /// one. The list comes from a page meant for people, so it is only
    /// trusted to leave out a candidate if it lists none at all or another
    /// candidate of the release.
    async fn check_on_web(&mut self, tags_known: bool) {
        let gh_api_client = &self.shared.gh_api_client;

        // Normally one or a few.
        let mut releases: Vec<&GhRelease> = Vec::new();
        for candidate in &self.candidates {
            if let Some(artifact) = &candidate.gh_artifact
                && !releases.contains(&&artifact.release)
            {
                releases.push(&artifact.release);
            }
        }

        // What github.com says about each candidate, by index.
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for release in releases {
            let (indices, names): (Vec<usize>, Vec<&str>) = self
                .candidates
                .iter()
                .enumerate()
                .filter_map(|(i, candidate)| {
                    let artifact = candidate.gh_artifact.as_ref()?;
                    (artifact.release == *release).then_some((i, artifact.artifact_name.as_str()))
                })
                .unzip();

            // If the git server could not tell, HEAD the release page first:
            // github.com answers a missing release with a 404 page about as
            // large as an asset list.
            let assets = if !tags_known && !gh_api_client.has_release_page(release).await {
                ReleaseAssets::NotFound
            } else {
                gh_api_client.release_assets_on_web(release, names).await
            };

            match assets {
                ReleaseAssets::NotFound | ReleaseAssets::NoAssets => missing.extend(indices),
                ReleaseAssets::Listed(listed) if !listed.is_empty() => {
                    for (k, i) in indices.into_iter().enumerate() {
                        if listed.contains(&k) {
                            found.push(i);
                        } else {
                            missing.push(i);
                        }
                    }
                }
                // It might not list every asset: check them some other way.
                ReleaseAssets::Listed(_) | ReleaseAssets::Unknown => (),
            }
        }

        for i in found {
            self.candidates[i].get_only = true;
        }
        // From the end, so that the indices left stay the same.
        missing.sort_unstable();
        for i in missing.into_iter().rev() {
            self.candidates.swap_remove(i);
        }
    }

    /// With a token, get the assets of the GitHub releases of the candidates
    /// github.com could not tell about, in one GraphQL query per
    /// repository, instead of one per release.
    async fn prefetch_gh_releases(&self) {
        let gh_api_client = &self.shared.gh_api_client;
        if !gh_api_client.has_gh_token() {
            return;
        }

        let releases = self
            .candidates
            .iter()
            .filter(|candidate| !candidate.get_only)
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
    async fn check(&self, candidate: Candidate) -> Result<Option<Resolved>, FetchError> {
        debug!("Checking for package at: '{}'", candidate.url);

        let gh_asset = match &candidate.gh_artifact {
            Some(artifact) if !candidate.get_only => self.check_gh_artifact(artifact).await?,
            _ => GhAsset::Unknown,
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

    /// Check a GitHub release asset with the API.
    async fn check_gh_artifact(&self, artifact: &GhReleaseArtifact) -> Result<GhAsset, FetchError> {
        loop {
            match get_gh_release_artifact_url(self.gh_api_client.clone(), artifact.clone()).await {
                Ok(Some(artifact_url)) => return Ok(GhAsset::Found(Some(artifact_url))),
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

    /// Resolve the candidates for `assets` of the public `repo`, returning
    /// the asset found, and how many candidates are left once the missing
    /// tags were dropped, and once github.com was asked.
    async fn probe(repo: &str, assets: &[&str]) -> (Option<String>, usize, usize) {
        let gh_api_client = GhApiClient::new(client(), None);
        let candidates = assets
            .iter()
            .map(|asset| new_candidate(repo, asset, None))
            .collect();
        let mut probe = new_probe(&gh_api_client, repo, candidates).await;

        assert!(probe.drop_missing_tags().await);
        let after_tags = probe.candidates.len();
        probe.check_on_web(true).await;
        let after_web = probe.candidates.len();

        (asset(probe.resolve().await), after_tags, after_web)
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
        let (found, after_tags, after_web) = probe(
            BINSTALL,
            &[
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
        // The missing tag is dropped, then the asset github.com doesn't
        // list, since it lists the other one.
        assert_eq!((after_tags, after_web), (2, 1));

        // github.com lists none of them: it might not list every asset, so
        // they are kept and checked some other way.
        let (found, after_tags, after_web) = probe(
            BINSTALL,
            &[
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.zip",
                "v0.20.1/cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
            ],
        )
        .await;
        assert_eq!(found, None);
        assert_eq!((after_tags, after_web), (2, 2));

        // A tag without a release: nothing to download.
        let (found, after_tags, after_web) = probe(
            BINSTALL,
            &["detect-targets-v0.1.0/detect-targets-x86_64-unknown-linux-musl.tgz"],
        )
        .await;
        assert_eq!(found, None);
        assert_eq!((after_tags, after_web), (1, 0));

        // The latest release, with `.full` builds and signatures.
        let (found, _, after_web) = probe(
            BINSTALL,
            &[
                "v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.tgz.sig",
                "v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.full.tgz",
                "v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.zip",
            ],
        )
        .await;
        assert!(found.is_some_and(|found| {
            found.starts_with("v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.")
        }));
        assert_eq!(after_web, 2);
    }

    #[tokio::test]
    async fn test_probe_cargo_quickinstall() {
        // A tag with `+`, and a target it has no build for.
        let (found, after_tags, after_web) = probe(
            QUICKINSTALL,
            &[
                "cargo-c-0.10.26+cargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-aarch64-unknown-linux-gnu.tar.gz",
                "cargo-c-0.10.26+cargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz",
                "cargo-c-v0.10.26+cargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz",
            ],
        )
        .await;
        assert_eq!(
            found.as_deref(),
            Some(
                "cargo-c-0.10.26+cargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz"
            )
        );
        assert_eq!((after_tags, after_web), (2, 1));

        // A tag that is a prefix of others, `ripgrep-14.1.10` and so on.
        let (found, after_tags, after_web) = probe(
            QUICKINSTALL,
            &[
                "ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz",
                "ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tgz",
                "ripgrep-14.1.10/ripgrep-14.1.10-x86_64-unknown-linux-gnu.tar.gz",
            ],
        )
        .await;
        assert_eq!(
            found.as_deref(),
            Some("ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!((after_tags, after_web), (2, 1));
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
        assert!(probe.drop_missing_tags().await);
        assert_eq!(probe.candidates.len(), 2);

        let found = probe.resolve().await.unwrap();
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
