use std::{
    fs::File,
    io::{self, BufReader, Read},
    path::PathBuf,
    sync::Arc,
};

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
    path: TempDir,
    repo: gix::ThreadSafeRepository,
    dl_template: CompactString,
}

impl GitIndex {
    fn new(url: GitUrl) -> Result<Self, BinstallError> {
        let tempdir = TempDir::new()?;

        let repo = Repository::shallow_clone(url, tempdir.as_ref())?.0;

        let mut v = Vec::with_capacity(100);
        File::open(tempdir.as_ref().join("config.json"))?.read_to_end(&mut v)?;

        let config: RegistryConfig = json_from_slice(&v).map_err(RegistryError::from)?;

        Ok(Self {
            path: tempdir,
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
        mut path: PathBuf,
        crate_name: &str,
        (c1, c2): &(CompactString, Option<CompactString>),
        version_req: &VersionReq,
    ) -> Result<MatchedVersion, BinstallError> {
        path.push(&**c1);
        if let Some(c2) = c2 {
            path.push(&**c2);
        }

        path.push(&*crate_name.to_lowercase());

        let f = File::open(path)
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => RegistryError::NotFound(crate_name.into()).into(),
                _ => BinstallError::from(e),
            })
            .map(BufReader::new)?;

        MatchedVersion::find(
            &mut JsonDeserializer::from_reader(f).into_iter(),
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
            let GitIndex { path, repo, dl_template } = this
                .0
                .git_index
                .get_or_try_init(|| GitIndex::new(this.0.url.clone()))?;

            let MatchedVersion { version, cksum } = Self::find_crate_matched_ver(
                path.as_ref().to_owned(),
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
