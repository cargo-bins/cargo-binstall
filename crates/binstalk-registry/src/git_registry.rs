use std::{io, path::PathBuf, sync::Arc};

use binstalk_downloader::{
    git::{GitCancellationToken, GitUrl, Repository},
    remote::Client,
};
use binstalk_types::cargo_toml_binstall::Meta;
use cargo_toml_workspace::cargo_toml::Manifest;
use compact_str::{CompactString, ToCompactString};
use once_cell::sync::OnceCell;
use semver::VersionReq;
use serde_json::{from_slice as json_from_slice, Deserializer as JsonDeserializer};
use tempfile::TempDir;
use tokio::task::spawn_blocking;
use url::Url;

use crate::{
    crate_prefix_components, parse_manifest, render_dl_template, MatchedVersion, RegistryConfig,
    RegistryError,
};

#[derive(Debug)]
struct GitIndex {
    _tempdir: TempDir,
    repo: Repository,
    dl_template: CompactString,
}

impl GitIndex {
    fn new(url: GitUrl, cancellation_token: GitCancellationToken) -> Result<Self, RegistryError> {
        let tempdir = TempDir::new()?;

        let repo = Repository::shallow_clone_bare(
            url.clone(),
            tempdir.as_ref(),
            Some(cancellation_token),
        )?;

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
    ) -> Result<MatchedVersion, RegistryError> {
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
    ) -> Result<Manifest<Meta>, RegistryError> {
        let crate_prefix = crate_prefix_components(name)?;
        let crate_name = name.to_compact_string();
        let version_req = version_req.clone();
        let this = self.clone();

        let cancellation_token = GitCancellationToken::default();
        // Cancel git operation if the future is cancelled (dropped).
        let cancel_on_drop = cancellation_token.clone().cancel_on_drop();

        let (matched_version, dl_url) = spawn_blocking(move || {
            let GitIndex {
                _tempdir: _,
                repo,
                dl_template,
            } = this
                .0
                .git_index
                .get_or_try_init(|| GitIndex::new(this.0.url.clone(), cancellation_token))?;

            let matched_version =
                Self::find_crate_matched_ver(repo, &crate_name, &crate_prefix, &version_req)?;

            let url = Url::parse(&render_dl_template(
                dl_template,
                &crate_name,
                &crate_prefix,
                &matched_version,
            )?)?;

            Ok::<_, RegistryError>((matched_version, url))
        })
        .await??;

        // Git operation done, disarm it
        cancel_on_drop.disarm();

        parse_manifest(client, name, dl_url, matched_version).await
    }
}
