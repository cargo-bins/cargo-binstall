use std::{env, sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::stream::Stream;
use log::debug;
use tokio::{
    sync::Mutex,
    time::{interval, Interval, MissedTickBehavior},
};

use crate::errors::BinstallError;

pub use reqwest::{tls, Method, RequestBuilder, Response};
pub use url::Url;

#[derive(Clone, Debug)]
pub struct Client {
    client: reqwest::Client,
    interval: Arc<Mutex<Interval>>,
}

impl Client {
    /// * `delay` - delay between launching next reqwests.
    pub fn new(min_tls: Option<tls::Version>, delay: Duration) -> Result<Self, BinstallError> {
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

        let mut interval = interval(delay);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        Ok(Self {
            client,
            interval: Arc::new(Mutex::new(interval)),
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
        self.interval.lock().await.tick().await;

        self.client
            .request(method.clone(), url.clone())
            .send()
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
