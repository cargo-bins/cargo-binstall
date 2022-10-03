use std::{env, sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::stream::Stream;
use log::debug;
use tokio::{
    sync::Mutex,
    time::{interval, Interval, MissedTickBehavior},
};

use crate::errors::BinstallError;

pub use reqwest::{tls, ClientBuilder, Method, RequestBuilder, Response};
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

        let mut builder = ClientBuilder::new()
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

    /// Wait until rate limiting permits us to launch
    /// the next request.
    async fn wait(&self) {
        self.interval.lock().await.tick().await;
    }

    pub async fn request(&self, method: Method, url: Url) -> RequestBuilder {
        self.wait().await;

        self.client.request(method, url)
    }

    pub async fn remote_exists(&self, url: Url, method: Method) -> Result<bool, BinstallError> {
        let req = self
            .request(method.clone(), url.clone())
            .await
            .send()
            .await
            .map_err(|err| BinstallError::Http { method, url, err })?;

        Ok(req.status().is_success())
    }

    pub async fn get_redirected_final_url(&self, url: Url) -> Result<Url, BinstallError> {
        let method = Method::HEAD;

        let req = self
            .request(method.clone(), url.clone())
            .await
            .send()
            .await
            .and_then(Response::error_for_status)
            .map_err(|err| BinstallError::Http { method, url, err })?;

        Ok(req.url().clone())
    }

    pub(crate) async fn create_request(
        &self,
        url: Url,
    ) -> Result<impl Stream<Item = reqwest::Result<Bytes>>, BinstallError> {
        debug!("Downloading from: '{url}'");

        let method = Method::GET;

        self.request(method.clone(), url.clone())
            .await
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|err| BinstallError::Http { method, url, err })
            .map(Response::bytes_stream)
    }
}
