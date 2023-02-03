use std::{
    num::NonZeroU64,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures_util::stream::{Stream, StreamExt};
use httpdate::parse_http_date;
use itertools::Itertools;
use reqwest::{
    header::{HeaderMap, RETRY_AFTER},
    Request, Response, StatusCode,
};
use thiserror::Error as ThisError;
use tokio::{sync::Mutex, time::Instant};
use tower::{limit::rate::RateLimit, Service, ServiceBuilder, ServiceExt};
use tracing::{debug, info};

pub use reqwest::{tls, Error as ReqwestError, Method};
pub use url::Url;

mod delay_request;
use delay_request::DelayRequest;

const MAX_RETRY_DURATION: Duration = Duration::from_secs(120);
const MAX_RETRY_COUNT: u8 = 3;
const DEFAULT_MIN_TLS: tls::Version = tls::Version::TLS_1_2;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),

    #[error(transparent)]
    Http(Box<HttpError>),
}

#[derive(Debug, ThisError)]
#[error("could not {method} {url}: {err}")]
pub struct HttpError {
    method: reqwest::Method,
    url: url::Url,
    #[source]
    err: reqwest::Error,
}

#[derive(Debug)]
struct Inner {
    client: reqwest::Client,
    service: Mutex<DelayRequest<RateLimit<reqwest::Client>>>,
}

#[derive(Clone, Debug)]
pub struct Client(Arc<Inner>);

impl Client {
    /// * `per` - must not be 0.
    /// * `num_request` - maximum number of requests to be processed for
    ///   each `per` duration.
    ///
    /// The Client created would use at least tls 1.2
    pub fn new(
        user_agent: impl AsRef<str>,
        min_tls: Option<tls::Version>,
        per: Duration,
        num_request: NonZeroU64,
    ) -> Result<Self, Error> {
        fn inner(
            user_agent: &str,
            min_tls: Option<tls::Version>,
            per: Duration,
            num_request: NonZeroU64,
        ) -> Result<Client, Error> {
            let tls_ver = min_tls
                .map(|tls| tls.max(DEFAULT_MIN_TLS))
                .unwrap_or(DEFAULT_MIN_TLS);

            let client = reqwest::ClientBuilder::new()
                .user_agent(user_agent)
                .https_only(true)
                .min_tls_version(tls_ver)
                .tcp_nodelay(false)
                .build()?;

            Ok(Client(Arc::new(Inner {
                client: client.clone(),
                service: Mutex::new(DelayRequest::new(
                    ServiceBuilder::new()
                        .rate_limit(num_request.get(), per)
                        .service(client),
                )),
            })))
        }

        inner(user_agent.as_ref(), min_tls, per, num_request)
    }

    /// Return inner reqwest client.
    pub fn get_inner(&self) -> &reqwest::Client {
        &self.0.client
    }

    async fn send_request_inner(
        &self,
        method: &Method,
        url: &Url,
    ) -> Result<Response, ReqwestError> {
        let mut count = 0;

        loop {
            let request = Request::new(method.clone(), url.clone());

            // Reduce critical section:
            //  - Construct the request before locking
            //  - Once the rate_limit is ready, call it and obtain
            //    the future, then release the lock before
            //    polling the future, which performs network I/O that could
            //    take really long.
            let future = self.0.service.lock().await.ready().await?.call(request);

            let response = future.await?;

            let status = response.status();

            match (status, parse_header_retry_after(response.headers())) {
                (
                    // 503                            429
                    StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS,
                    Some(duration),
                ) if duration <= MAX_RETRY_DURATION && count < MAX_RETRY_COUNT => {
                    info!("Receiver status code {status}, will wait for {duration:#?} and retry");

                    let deadline = Instant::now() + duration;

                    self.0
                        .service
                        .lock()
                        .await
                        .add_urls_to_delay([url, response.url()].into_iter().dedup(), deadline);
                }
                _ => break Ok(response),
            }

            count += 1;
        }
    }

    async fn send_request(
        &self,
        method: Method,
        url: Url,
        error_for_status: bool,
    ) -> Result<Response, Error> {
        self.send_request_inner(&method, &url)
            .await
            .and_then(|response| {
                if error_for_status {
                    response.error_for_status()
                } else {
                    Ok(response)
                }
            })
            .map_err(|err| Error::Http(Box::new(HttpError { method, url, err })))
    }

    /// Check if remote exists using `method`.
    pub async fn remote_exists(&self, url: Url, method: Method) -> Result<bool, Error> {
        Ok(self
            .send_request(method, url, false)
            .await?
            .status()
            .is_success())
    }

    /// Attempt to get final redirected url.
    pub async fn get_redirected_final_url(&self, url: Url) -> Result<Url, Error> {
        Ok(self
            .send_request(Method::HEAD, url, true)
            .await?
            .url()
            .clone())
    }

    /// Create `GET` request to `url` and return a stream of the response data.
    /// On status code other than 200, it will return an error.
    pub async fn get_stream(
        &self,
        url: Url,
    ) -> Result<impl Stream<Item = Result<Bytes, Error>>, Error> {
        debug!("Downloading from: '{url}'");

        self.send_request(Method::GET, url, true)
            .await
            .map(|response| response.bytes_stream().map(|res| res.map_err(Error::from)))
    }
}

fn parse_header_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let header = headers
        .get_all(RETRY_AFTER)
        .into_iter()
        .last()?
        .to_str()
        .ok()?;

    match header.parse::<u64>() {
        Ok(dur) => Some(Duration::from_secs(dur)),
        Err(_) => {
            let system_time = parse_http_date(header).ok()?;

            let retry_after_unix_timestamp =
                system_time.duration_since(SystemTime::UNIX_EPOCH).ok()?;

            let curr_time_unix_timestamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("SystemTime before UNIX EPOCH!");

            // retry_after_unix_timestamp - curr_time_unix_timestamp
            // If underflows, returns Duration::ZERO.
            Some(retry_after_unix_timestamp.saturating_sub(curr_time_unix_timestamp))
        }
    }
}
