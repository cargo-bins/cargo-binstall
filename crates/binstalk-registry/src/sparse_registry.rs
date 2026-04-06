use binstalk_downloader::remote::{Client, StatusCode};
use binstalk_types::cargo_toml_binstall::Meta;
use cargo_toml_workspace::cargo_toml::Manifest;
use compact_str::CompactString;
use semver::VersionReq;
use serde_json::Deserializer as JsonDeserializer;
use tokio::sync::OnceCell;
use tracing::instrument;
use url::Url;

use crate::{
    apply_auth, crate_prefix_components, parse_manifest, render_dl_template, MatchedVersion,
    RegistryAuth, RegistryConfig, RegistryError,
};

#[derive(Debug)]
pub struct SparseRegistry {
    url: Url,
    config: OnceCell<RegistryConfig>,
}

impl SparseRegistry {
    /// * `url` - `url.cannot_be_a_base()` must be `false`
    pub fn new(url: Url) -> Self {
        Self {
            url,
            config: Default::default(),
        }
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    async fn get_config(
        &self,
        client: &Client,
        auth: Option<&RegistryAuth>,
    ) -> Result<&RegistryConfig, RegistryError> {
        self.config
            .get_or_try_init(|| {
                Box::pin(async {
                    let mut url = self.url.clone();
                    url.path_segments_mut().unwrap().push("config.json");

                    let response = client.get(url.clone()).send(false).await?;
                    let response = match response.status() {
                        StatusCode::UNAUTHORIZED => {
                            let Some(auth) = auth else {
                                return Err(RegistryError::AuthenticationRequired(Box::new(url)));
                            };

                            apply_auth(client.get(url), Some(auth)).send(true).await?
                        }
                        _ => response.error_for_status()?,
                    };

                    response.json().await.map_err(RegistryError::from)
                })
            })
            .await
    }

    /// `url` must be a valid http(s) url.
    async fn find_crate_matched_ver(
        client: &Client,
        mut url: Url,
        auth: Option<&RegistryAuth>,
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

        let response = apply_auth(client.get(url.clone()), auth)
            .send(false)
            .await?;
        let body = match response.status() {
            StatusCode::OK => response.bytes().await.map_err(RegistryError::from)?,
            StatusCode::NOT_FOUND
            | StatusCode::GONE
            | StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS => {
                return Err(RegistryError::NotFound(crate_name.into()));
            }
            StatusCode::UNAUTHORIZED => {
                return Err(RegistryError::AuthenticationRequired(Box::new(url)));
            }
            _ => response
                .error_for_status()?
                .bytes()
                .await
                .map_err(RegistryError::from)?,
        };
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
        auth: Option<&RegistryAuth>,
        crate_name: &str,
        version_req: &VersionReq,
    ) -> Result<Manifest<Meta>, RegistryError> {
        let crate_prefix = crate_prefix_components(crate_name)?;
        let registry_config = self.get_config(&client, auth).await?;
        let auth = if registry_config.auth_required {
            let Some(auth) = auth else {
                return Err(RegistryError::AuthenticationRequired(Box::new(
                    self.url.clone(),
                )));
            };

            Some(auth)
        } else {
            None
        };
        let matched_version = Self::find_crate_matched_ver(
            &client,
            self.url.clone(),
            auth,
            crate_name,
            &crate_prefix,
            version_req,
        )
        .await?;
        let dl_url = Url::parse(&render_dl_template(
            &registry_config.dl,
            crate_name,
            &crate_prefix,
            &matched_version,
        )?)?;

        parse_manifest(client, crate_name, dl_url, matched_version, auth).await
    }
}
