use std::time::Duration;

use binstalk_downloader::remote::{Client, StatusCode};
use compact_str::CompactString;
use jiff::Timestamp;
use semver::VersionReq;
use serde_json::Deserializer as JsonDeserializer;
use tokio::sync::OnceCell;
use tracing::instrument;
use url::Url;

use crate::{
    MatchedCrate, MatchedVersion, RegistryAuth, RegistryConfig, RegistryError, apply_auth,
    crate_prefix_components, publish_age::publish_cutoff, render_dl_template,
};

pub(crate) const CRATES_IO_SPARSE_INDEX: &str = "https://index.crates.io/";
/// `dl` in <https://index.crates.io/config.json>
const CRATES_IO_DL: &str = "https://static.crates.io/crates";

#[derive(Debug)]
pub struct SparseRegistry {
    url: Url,
    config: OnceCell<RegistryConfig>,
}

impl SparseRegistry {
    /// * `url` - `url.cannot_be_a_base()` must be `false`
    pub fn new(url: Url) -> Self {
        // crates.io's `config.json` is well known, and its `static.crates.io`
        // download urls are relied upon far beyond cargo, so use it directly
        // instead of fetching it on every install.
        // `test_crates_io_config_preset_up_to_date` catches a change.
        let config = if url.as_str() == CRATES_IO_SPARSE_INDEX {
            OnceCell::new_with(Some(RegistryConfig {
                dl: CompactString::const_new(CRATES_IO_DL),
                auth_required: false,
            }))
        } else {
            OnceCell::new()
        };

        Self { url, config }
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
        cutoff: Option<Timestamp>,
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
            cutoff,
        )
    }

    #[instrument(
        skip(self, client, version_req),
        fields(
            registry_url = format_args!("{}", self.url),
            version_req = format_args!("{version_req}"),
        )
    )]
    pub async fn find_crate_matched(
        &self,
        client: Client,
        auth: Option<&RegistryAuth>,
        crate_name: &str,
        version_req: &VersionReq,
        min_publish_age: Option<Duration>,
    ) -> Result<MatchedCrate, RegistryError> {
        let cutoff = min_publish_age.and_then(|min_age| publish_cutoff(Timestamp::now(), min_age));
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
            cutoff,
        )
        .await?;
        let dl_url = Url::parse(&render_dl_template(
            &registry_config.dl,
            crate_name,
            &crate_prefix,
            &matched_version,
        )?)?;

        Ok(MatchedCrate::new(
            client,
            crate_name,
            dl_url,
            matched_version,
            auth,
        ))
    }
}

#[cfg(test)]
mod test {
    use std::num::NonZeroU16;

    use super::*;
    use crate::Registry;

    fn crates_io_config(registry: &Registry) -> Option<&RegistryConfig> {
        match registry {
            Registry::Sparse(sparse) => sparse.config.get(),
            #[allow(unreachable_patterns)]
            _ => panic!("expected sparse registry, got {registry:?}"),
        }
    }

    #[test]
    fn test_crates_io_config_preset() {
        for registry in [
            Registry::crates_io_sparse_registry(),
            "sparse+https://index.crates.io".parse().unwrap(),
            "sparse+https://index.crates.io/".parse().unwrap(),
        ] {
            let config = crates_io_config(&registry).unwrap();
            assert_eq!(config.dl, CRATES_IO_DL);
            assert!(!config.auth_required);
        }

        let other: Registry = "sparse+https://example.com/index/".parse().unwrap();
        assert!(crates_io_config(&other).is_none());
    }

    /// Fail if crates.io ever changes its `config.json`, so the preset can be
    /// updated.
    #[tokio::test]
    async fn test_crates_io_config_preset_up_to_date() {
        let client = Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
            NonZeroU16::new(10).unwrap(),
            1.try_into().unwrap(),
            [],
        )
        .unwrap();

        let live = SparseRegistry {
            url: Url::parse(CRATES_IO_SPARSE_INDEX).unwrap(),
            config: OnceCell::new(),
        };
        let live = live.get_config(&client, None).await.unwrap();

        assert_eq!(live.dl, CRATES_IO_DL);
        assert!(!live.auth_required);
    }
}
