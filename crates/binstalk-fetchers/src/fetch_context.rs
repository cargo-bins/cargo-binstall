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
    sync::{Arc, RwLock},
};

use binstalk_git_repo_api::{
    gh_api_client::{GhRelease, ReleaseArtifacts, ReleaseAssetsPage},
    git_refs::RepoTags,
};
use compact_str::CompactString;
use tokio::sync::{Mutex, OnceCell};
use url::Url;

use crate::RepoInfo;

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

    #[cfg(test)]
    pub(crate) fn release_assets_pages_len(&self) -> usize {
        self.release_assets_pages.len()
    }

    #[cfg(test)]
    pub(crate) fn release_artifacts_len(&self) -> usize {
        self.release_artifacts.len()
    }
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
}
