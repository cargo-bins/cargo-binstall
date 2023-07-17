use std::{path::PathBuf, sync::Arc};

use cargo_toml::Manifest;
use compact_str::{CompactString, ToCompactString};
use once_cell::sync::OnceCell;
use semver::VersionReq;
use serde_json::{from_slice as json_from_slice, Deserializer as JsonDeserializer};
use tempfile::TempDir;
use tokio::task::spawn_blocking;
use url::Url;

use crate::helpers::git::GitError;
use crate::{
    drivers::registry::{
        crate_prefix_components, parse_manifest, render_dl_template, MatchedVersion,
        RegistryConfig, RegistryError,
    },
    errors::BinstallError,
    helpers::{
        git::{GitUrl, Repository},
        remote::Client,
    },
    manifests::cargo_toml_binstall::Meta,
};

#[derive(Debug)]
struct GitIndex {
    _tempdir: TempDir,
    repo: gix::ThreadSafeRepository,
    dl_template: CompactString,
}

impl GitIndex {
    fn new(url: GitUrl) -> Result<Self, BinstallError> {
        let tempdir = TempDir::new()?;

        let repo = Repository::shallow_clone(url, tempdir.as_ref())?.0;

        let config: RegistryConfig = {
            let config = repo
                .head_commit()
                .map_err(GitError::from)?
                .tree()
                .map_err(GitError::from)?
                .lookup_entry_by_path("config.json")
                .expect("root-lookups can't fail as object is parsed")
                .ok_or(GitError::MissingConfigJson)?
                .object()
                .map_err(GitError::from)?;
            json_from_slice(&config.data).map_err(RegistryError::from)?
        };

        Ok(Self {
            _tempdir: tempdir,
            repo: repo.into(),
            dl_template: config.dl,
        })
    }
}

#[derive(Debug)]
struct GitRegistryInner {
    url: GitUrl,
    git_index: OnceCell<GitIndex>,
}

#[derive(Clone, Debug)]
pub struct GitRegistry(Arc<GitRegistryInner>);

impl GitRegistry {
    pub fn new(url: GitUrl) -> Self {
        Self(Arc::new(GitRegistryInner {
            url,
            git_index: Default::default(),
        }))
    }

    /// WARNING: This is a blocking operation.
    fn find_crate_matched_ver(
        repo: &gix::Repository,
        crate_name: &str,
        (c1, c2): &(CompactString, Option<CompactString>),
        version_req: &VersionReq,
    ) -> Result<MatchedVersion, BinstallError> {
        let mut path = PathBuf::with_capacity(128);
        path.push(&**c1);
        if let Some(c2) = c2 {
            path.push(&**c2);
        }

        path.push(&*crate_name.to_lowercase());
        let crate_versions = repo
            .head_commit()
            .map_err(GitError::from)?
            .tree()
            .map_err(GitError::from)?
            .lookup_entry_by_path(path)
            .map_err(GitError::from)?
            .ok_or_else(|| RegistryError::NotFound(crate_name.into()))?
            .object()
            .map_err(GitError::from)?;

        MatchedVersion::find(
            &mut JsonDeserializer::from_slice(&crate_versions.data).into_iter(),
            version_req,
        )
    }

    pub async fn fetch_crate_matched(
        &self,
        client: Client,
        name: &str,
        version_req: &VersionReq,
    ) -> Result<Manifest<Meta>, BinstallError> {
        let crate_prefix = crate_prefix_components(name)?;
        let crate_name = name.to_compact_string();
        let version_req = version_req.clone();
        let this = self.clone();

        let (version, dl_url) = spawn_blocking(move || {
            let GitIndex {
                _tempdir: _,
                repo,
                dl_template,
            } = this
                .0
                .git_index
                .get_or_try_init(|| GitIndex::new(this.0.url.clone()))?;

            let MatchedVersion { version, cksum } = Self::find_crate_matched_ver(
                &repo.to_thread_local(),
                &crate_name,
                &crate_prefix,
                &version_req,
            )?;

            let url = Url::parse(&render_dl_template(
                dl_template,
                &crate_name,
                &crate_prefix,
                &version,
                &cksum,
            )?)?;

            Ok::<_, BinstallError>((version, url))
        })
        .await??;

        parse_manifest(client, name, &version, dl_url).await
    }
}
