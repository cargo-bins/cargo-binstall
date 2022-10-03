use std::{env, num::NonZeroU64, sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::stream::Stream;
use log::debug;
use reqwest::{Request, Response};
use tokio::sync::Mutex;
use tower::{limit::rate::RateLimit, Service, ServiceBuilder, ServiceExt};

use crate::errors::BinstallError;

pub use reqwest::{tls, Method};
pub use url::Url;

#[derive(Clone, Debug)]
pub struct Client {
    client: reqwest::Client,
    rate_limit: Arc<Mutex<RateLimit<reqwest::Client>>>,
}

impl Client {
    /// * `per` - must not be 0.
    pub fn new(
        min_tls: Option<tls::Version>,
        per: Duration,
        num_request: NonZeroU64,
    ) -> Result<Self, BinstallError> {
        const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

        let mut builder = reqwest::ClientBuilder::new()
            .user_agent(USER_AGENT)
            .https_only(true)
            .min_tls_version(tls::Version::TLS_1_2)
            .tcp_nodelay(false);

        if let Some(ver) = min_tls {
            builder = builder.min_tls_version(ver);
        }

        let client = builder.build()?;

        Ok(Self {
            client: client.clone(),
            rate_limit: Arc::new(Mutex::new(
                ServiceBuilder::new()
                    .rate_limit(num_request.get(), per)
                    .service(client),
            )),
        })
    }

    pub fn get_inner(&self) -> &reqwest::Client {
        &self.client
    }

    async fn send_request(
        &self,
        method: Method,
        url: Url,
        error_for_status: bool,
    ) -> Result<Response, BinstallError> {
        let mut rate_limit = self.rate_limit.lock().await;

        rate_limit
            .ready()
            .await?
            .call(Request::new(method.clone(), url.clone()))
            .await
            .and_then(|response| {
                if error_for_status {
                    response.error_for_status()
                } else {
                    Ok(response)
                }
            })
            .map_err(|err| BinstallError::Http { method, url, err })
    }

    pub async fn remote_exists(&self, url: Url, method: Method) -> Result<bool, BinstallError> {
        Ok(self
            .send_request(method, url, false)
            .await?
            .status()
            .is_success())
    }

    pub async fn get_redirected_final_url(&self, url: Url) -> Result<Url, BinstallError> {
        Ok(self
            .send_request(Method::HEAD, url, true)
            .await?
            .url()
            .clone())
    }

    pub(crate) async fn create_request(
        &self,
        url: Url,
    ) -> Result<impl Stream<Item = reqwest::Result<Bytes>>, BinstallError> {
        debug!("Downloading from: '{url}'");

        self.send_request(Method::GET, url, true)
            .await
            .map(Response::bytes_stream)
    }
}
