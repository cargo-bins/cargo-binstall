use std::{io, path::PathBuf, sync::Arc};

use cargo_toml::Manifest;
use compact_str::{CompactString, ToCompactString};
use once_cell::sync::OnceCell;
use semver::VersionReq;
use serde_json::{from_slice as json_from_slice, Deserializer as JsonDeserializer};
use tempfile::TempDir;
use tokio::task::spawn_blocking;
use url::Url;

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
    repo: Repository,
    dl_template: CompactString,
}

impl GitIndex {
    fn new(url: GitUrl) -> Result<Self, BinstallError> {
        let tempdir = TempDir::new()?;

        let repo = Repository::shallow_clone_bare(url.clone(), tempdir.as_ref())?;

        let config: RegistryConfig = {
            let config = repo
                .get_head_commit_entry_data_by_path("config.json")?
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("config.json not found in repository `{url}`"),
                    )
                })?;

            json_from_slice(&config).map_err(RegistryError::from)?
        };

        Ok(Self {
            _tempdir: tempdir,
            repo,
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
        repo: &Repository,
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
            .get_head_commit_entry_data_by_path(path)?
            .ok_or_else(|| RegistryError::NotFound(crate_name.into()))?;

        MatchedVersion::find(
            &mut JsonDeserializer::from_slice(&crate_versions).into_iter(),
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

        let (matched_version, dl_url) = spawn_blocking(move || {
            let GitIndex {
                _tempdir: _,
                repo,
                dl_template,
            } = this
                .0
                .git_index
                .get_or_try_init(|| GitIndex::new(this.0.url.clone()))?;

            let matched_version =
                Self::find_crate_matched_ver(repo, &crate_name, &crate_prefix, &version_req)?;

            let url = Url::parse(&render_dl_template(
                dl_template,
                &crate_name,
                &crate_prefix,
                &matched_version,
            )?)?;

            Ok::<_, BinstallError>((matched_version, url))
        })
        .await??;

        parse_manifest(client, name, dl_url, matched_version).await
    }
}
