use binstalk_downloader::remote::{Client, Error as RemoteError};
use binstalk_types::cargo_toml_binstall::Meta;
use cargo_toml_workspace::cargo_toml::Manifest;
use compact_str::CompactString;
use semver::VersionReq;
use serde_json::Deserializer as JsonDeserializer;
use tokio::sync::OnceCell;
use tracing::instrument;
use url::Url;

use crate::{
    crate_prefix_components, parse_manifest, render_dl_template, MatchedVersion, RegistryConfig,
    RegistryError,
};

#[derive(Debug)]
pub struct SparseRegistry {
    url: Url,
    dl_template: OnceCell<CompactString>,
}

impl SparseRegistry {
    /// * `url` - `url.cannot_be_a_base()` must be `false`
    pub fn new(url: Url) -> Self {
        Self {
            url,
            dl_template: Default::default(),
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    async fn get_dl_template(&self, client: &Client) -> Result<&str, RegistryError> {
        self.dl_template
            .get_or_try_init(|| {
                Box::pin(async {
                    let mut url = self.url.clone();
                    url.path_segments_mut().unwrap().push("config.json");
                    let config: RegistryConfig = client.get(url).send(true).await?.json().await?;
                    Ok(config.dl)
                })
            })
            .await
            .map(AsRef::as_ref)
    }

    /// `url` must be a valid http(s) url.
    async fn find_crate_matched_ver(
        client: &Client,
        mut url: Url,
        crate_name: &str,
        (c1, c2): &(CompactString, Option<CompactString>),
        version_req: &VersionReq,
    ) -> Result<MatchedVersion, RegistryError> {
        {
            let mut path = url.path_segments_mut().unwrap();

            path.push(c1);
            if let Some(c2) = c2 {
                path.push(c2);
            }

            path.push(&crate_name.to_lowercase());
        }

        let body = client
            .get(url)
            .send(true)
            .await
            .map_err(|e| match e {
                RemoteError::Http(e) if e.is_status() => RegistryError::NotFound(crate_name.into()),
                e => e.into(),
            })?
            .bytes()
            .await
            .map_err(RegistryError::from)?;
        MatchedVersion::find(
            &mut JsonDeserializer::from_slice(&body).into_iter(),
            version_req,
        )
    }

    #[instrument(
        skip(self, client, version_req),
        fields(
            registry_url = format_args!("{}", self.url),
            version_req = format_args!("{version_req}"),
        )
    )]
    pub async fn fetch_crate_matched(
        &self,
        client: Client,
        crate_name: &str,
        version_req: &VersionReq,
    ) -> Result<Manifest<Meta>, RegistryError> {
        let crate_prefix = crate_prefix_components(crate_name)?;
        let dl_template = self.get_dl_template(&client).await?;
        let matched_version = Self::find_crate_matched_ver(
            &client,
            self.url.clone(),
            crate_name,
            &crate_prefix,
            version_req,
        )
        .await?;
        let dl_url = Url::parse(&render_dl_template(
            dl_template,
            crate_name,
            &crate_prefix,
            &matched_version,
        )?)?;

        parse_manifest(client, crate_name, dl_url, matched_version).await
    }
}
