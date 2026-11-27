//! What the fetchers learn about repositories, shared by all crates
//! resolved in one run (see [`FetchContext`]): by all targets of a crate,
//! which are tried one after another, by their overrides, under both the
//! names a crate is looked up with (see [`Data::with_name`](crate::Data::with_name)),
//! and by other crates in the same repository, such as a workspace's crates
//! released together.
//!
//! [`binstalk_git_repo_api`] caches nothing, so nothing outlives the
//! [`FetchContext`].

use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    fmt,
    hash::Hash,
    path::Path,
    sync::{Arc, RwLock},
};

use binstalk_downloader::download::ExtractedFiles;
use binstalk_git_repo_api::{
    gh_api_client::{GhRelease, ReleaseArtifacts, ReleaseAssetsPage},
    git_refs::RepoTags,
};
use compact_str::CompactString;
use tokio::sync::{Mutex, OnceCell};
use url::Url;

use binstalk_types::cargo_toml_binstall::{PkgFmt, SigningAlgorithm};

use crate::{Extracted, FetchError, RepoInfo};

/// Shared by the fetchers of all crates resolved together, through
/// [`Data::new`](crate::Data::new), so that what one crate's fetchers asked
/// about a repository isn't asked again for another crate.
#[derive(Default)]
pub struct FetchContext {
    /// By the `repository` of the crates' manifests.
    repo_infos: Cells<String, OnceCell<Option<RepoInfo>>>,
    /// By [`RepoInfo::repo`], so that crates whose `repository` points at
    /// a subdirectory of the same repository share them.
    ///
    /// Locked while the git server is asked, so a tag is asked once.
    repo_tags: Cells<Url, Mutex<Tags>>,
    release_assets_pages: Cells<GhRelease, OnceCell<ReleaseAssetsPage>>,
    release_artifacts: Cells<GhRelease, OnceCell<Option<ReleaseArtifacts>>>,
    /// Whether a package exists, by its URL, from the GET that checked it,
    /// so that crates in the same package send one GET for it, see
    /// [`FetchContext::check_once`].
    package_urls: Cells<Url, OnceCell<bool>>,
    /// Packages extracted so far, so that crates in the same package
    /// download and extract it once.
    packages: Cells<PackageId, OnceCell<Arc<Extracted>>>,
}

impl fmt::Debug for FetchContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchContext").finish_non_exhaustive()
    }
}

impl FetchContext {
    /// Where what is known about the repository `repo` is kept, `None` if
    /// it is no repository the fetchers can use.
    pub(crate) fn repo_info(&self, repo: &str) -> Arc<OnceCell<Option<RepoInfo>>> {
        self.repo_infos.get(repo)
    }

    /// Which release tags exist in `repo`, see [`RepoInfo::repo`].
    pub(crate) fn repo_tags(&self, repo: &Url) -> Arc<Mutex<Tags>> {
        self.repo_tags.get(repo)
    }

    /// Where github.com's asset list of `release` is kept.
    pub(crate) fn release_assets_page(
        &self,
        release: &GhRelease,
    ) -> Arc<OnceCell<ReleaseAssetsPage>> {
        self.release_assets_pages.get(release)
    }

    /// Where the API's answer about the assets of `release` is kept, `None`
    /// if there is no such release.
    pub(crate) fn release_artifacts(
        &self,
        release: &GhRelease,
    ) -> Arc<OnceCell<Option<ReleaseArtifacts>>> {
        self.release_artifacts.get(release)
    }

    /// Check whether the package at `url` exists with `check`, unless it
    /// was checked for another crate already, or is being checked: then
    /// that answer is returned once ready.
    ///
    /// `check` gives whether it exists, and is the only one to see the
    /// response it gets, to download the package from: the crates that
    /// get the answer from another one download it, if at all, through
    /// [`FetchContext::extract_once`], which the one that checked it
    /// normally is in already.
    ///
    /// If checking it for another crate failed, it is tried again.
    pub(crate) async fn check_once<F>(&self, url: &Url, check: F) -> Result<bool, FetchError>
    where
        F: AsyncFnOnce() -> Result<bool, FetchError>,
    {
        self.package_urls
            .get(url)
            .get_or_try_init(async || check().await)
            .await
            .copied()
    }

    /// Extract the package `id` to `dst` with `extract`, unless it was
    /// extracted for another crate already, or is being extracted: then
    /// that one is returned once ready.
    ///
    /// If extracting it for another crate failed, it is tried again.
    pub(crate) async fn extract_once<F>(
        &self,
        id: PackageId,
        dst: &Path,
        extract: F,
    ) -> Result<Arc<Extracted>, FetchError>
    where
        F: AsyncFnOnce() -> Result<ExtractedFiles, FetchError>,
    {
        self.packages
            .get(&id)
            .get_or_try_init(async || {
                let files = extract().await?;
                Ok(Arc::new(Extracted::new(dst.to_path_buf(), files)))
            })
            .await
            .cloned()
    }

    #[cfg(test)]
    /// How many of github.com's asset lists were fetched.
    pub(crate) fn release_assets_pages_len(&self) -> usize {
        self.release_assets_pages
            .0
            .read()
            .unwrap()
            .values()
            .filter(|cell| cell.initialized())
            .count()
    }

    #[cfg(test)]
    pub(crate) fn release_artifacts_len(&self) -> usize {
        self.release_artifacts.len()
    }
}

/// What [`FetchContext::extract_once`] extracts: two packages with the same id
/// extract to the same files, and only after the same verification, so a
/// package checked against a signing key isn't given to a crate that would
/// check it against another key or not at all.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct PackageId {
    pub(crate) url: Url,
    pub(crate) pkg_fmt: PkgFmt,
    /// The algorithm and public key its signature is checked with, if it is.
    pub(crate) signing: Option<(SigningAlgorithm, Box<str>)>,
}

/// Which release tags exist, from the repository's git server.
#[derive(Debug, Default)]
pub(crate) struct Tags {
    /// Every tag asked about so far.
    known: Option<RepoTags>,
    /// The git server could not tell, so it isn't asked again.
    failed: bool,
}

impl Tags {
    /// The ones of `tags` the git server wasn't asked about yet, none if it
    /// can't tell.
    pub(crate) fn unasked<'a>(
        &self,
        tags: impl IntoIterator<Item = &'a str>,
    ) -> HashSet<CompactString> {
        if self.failed {
            return HashSet::new();
        }
        tags.into_iter()
            .filter(|tag| self.exists(tag).is_none())
            .map(CompactString::from)
            .collect()
    }

    /// Keep what the git server answered, `None` if it couldn't tell.
    pub(crate) fn add(&mut self, tags: Option<RepoTags>) {
        match (tags, &mut self.known) {
            (None, _) => self.failed = true,
            (Some(tags), Some(known)) => known.merge(tags),
            (Some(tags), known @ None) => *known = Some(tags),
        }
    }

    /// Whether `tag` exists, `None` if the git server wasn't asked about it.
    pub(crate) fn exists(&self, tag: &str) -> Option<bool> {
        self.known.as_ref()?.exists(tag)
    }
}

/// A `V` per key, created on first use and shared by everyone who looks it
/// up, so that what it holds is fetched once however many look it up at
/// once.
struct Cells<K, V>(RwLock<HashMap<K, Arc<V>>>);

impl<K, V> Default for Cells<K, V> {
    fn default() -> Self {
        Self(RwLock::default())
    }
}

impl<K: Eq + Hash, V: Default> Cells<K, V> {
    fn get<Q>(&self, key: &Q) -> Arc<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ToOwned<Owned = K> + ?Sized,
    {
        if let Some(cell) = self.0.read().unwrap().get(key) {
            return Arc::clone(cell);
        }
        Arc::clone(self.0.write().unwrap().entry(key.to_owned()).or_default())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.read().unwrap().len()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_tags_failed() {
        let mut tags = Tags::default();
        assert_eq!(tags.unasked(["v1.0.0"]).len(), 1);
        assert_eq!(tags.exists("v1.0.0"), None);

        // The git server couldn't tell: it isn't asked again.
        tags.add(None);
        assert!(tags.unasked(["v1.0.0", "v2.0.0"]).is_empty());
        assert_eq!(tags.exists("v1.0.0"), None);
    }

    /// Crates in the same package check it once, and again only if that
    /// failed.
    #[tokio::test]
    async fn test_check_once() {
        use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

        let context = FetchContext::default();
        let url = Url::parse("https://example.com/pkg.tgz").unwrap();
        let checks = AtomicUsize::new(0);

        let failing = async || {
            checks.fetch_add(1, Relaxed);
            Err(FetchError::InvalidSignature)
        };
        assert!(context.check_once(&url, failing).await.is_err());

        let check = async || {
            checks.fetch_add(1, Relaxed);
            tokio::task::yield_now().await;
            Ok(true)
        };
        let (a, b) = tokio::join!(
            context.check_once(&url, check),
            context.check_once(&url, check)
        );
        assert!(a.unwrap() && b.unwrap());
        assert_eq!(checks.load(Relaxed), 2);

        let other = Url::parse("https://example.com/other.tgz").unwrap();
        let missing = async || Ok(false);
        assert!(!context.check_once(&other, missing).await.unwrap());
        assert!(context.check_once(&url, check).await.unwrap());
        assert_eq!(checks.load(Relaxed), 2);
    }
}
