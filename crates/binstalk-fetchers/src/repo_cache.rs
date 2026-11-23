//! What the fetchers learn about a crate's repository, kept for as long as
//! the crate is being resolved: by all its targets, which are tried one
//! after another, and under both the names it is looked up with, see
//! [`Data::with_name`](crate::Data::with_name).
//!
//! [`binstalk_git_repo_api`] caches nothing, so nothing outlives the crate.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use binstalk_git_repo_api::gh_api_client::{
    GhRelease, ReleaseArtifacts, ReleaseAssets, ReleaseAssetsPage,
};
use tokio::sync::OnceCell;

use crate::{RepoInfo, common::Client};

/// The API's answer about a release's assets, `None` if there is no such
/// release.
pub(crate) type ReleaseArtifactsCell = OnceCell<Option<ReleaseArtifacts>>;

#[derive(Debug, Default)]
pub(crate) struct RepoCache {
    pub(crate) info: OnceCell<Option<RepoInfo>>,
    /// github.com's asset lists of the releases looked up. Only one target
    /// at a time looks them up, no need to wait for one being fetched.
    release_assets: Mutex<Vec<(GhRelease, ReleaseAssetsPage)>>,
    /// Checked by all candidates of a target at once, so the first one asks
    /// and the others wait for it.
    release_artifacts: Mutex<HashMap<GhRelease, Arc<ReleaseArtifactsCell>>>,
}

impl RepoCache {
    /// Which of the asset `names` of `release` github.com lists, getting its
    /// page if no target did yet.
    pub(crate) async fn release_assets_on_web<'a>(
        &self,
        client: &Client,
        release: GhRelease,
        names: impl IntoIterator<Item = &'a str>,
    ) -> ReleaseAssets {
        if let Some((_, page)) = self
            .release_assets
            .lock()
            .unwrap()
            .iter()
            .find(|(r, _)| *r == release)
        {
            return page.lists(names);
        }

        let page = ReleaseAssetsPage::fetch(client, &release).await;
        let assets = page.lists(names);
        self.release_assets.lock().unwrap().push((release, page));
        assets
    }

    /// Where the API's answer about the assets of `release` is kept.
    pub(crate) fn release_artifacts(&self, release: &GhRelease) -> Arc<ReleaseArtifactsCell> {
        let mut cells = self.release_artifacts.lock().unwrap();
        match cells.get(release) {
            Some(cell) => Arc::clone(cell),
            None => Arc::clone(cells.entry(release.clone()).or_default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn release_assets_len(&self) -> usize {
        self.release_assets.lock().unwrap().len()
    }
}
