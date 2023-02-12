use std::{
    num::{NonZeroU64, NonZeroU8},
    ops::ControlFlow,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures_lite::stream::{Stream, StreamExt};
use httpdate::parse_http_date;
use reqwest::{
    header::{HeaderMap, RETRY_AFTER},
    Request, Response, StatusCode,
};
use thiserror::Error as ThisError;
use tower::{limit::rate::RateLimit, Service, ServiceBuilder, ServiceExt};
use tracing::{debug, info};

pub use reqwest::{tls, Error as ReqwestError, Method};
pub use url::Url;

mod delay_request;
use delay_request::DelayRequest;

const MAX_RETRY_DURATION: Duration = Duration::from_secs(120);
const MAX_RETRY_COUNT: u8 = 3;
const DEFAULT_RETRY_DURATION_FOR_RATE_LIMIT: Duration = Duration::from_millis(200);
const RETRY_DURATION_FOR_TIMEOUT: Duration = Duration::from_millis(200);
const DEFAULT_MIN_TLS: tls::Version = tls::Version::TLS_1_2;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("Reqwest error: {0}")]
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
    service: DelayRequest<RateLimit<reqwest::Client>>,
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
                service: DelayRequest::new(
                    ServiceBuilder::new()
                        .rate_limit(num_request.get(), per)
                        .service(client),
                ),
            })))
        }

        inner(user_agent.as_ref(), min_tls, per, num_request)
    }

    /// Return inner reqwest client.
    pub fn get_inner(&self) -> &reqwest::Client {
        &self.0.client
    }

    /// Return `Err(_)` for fatal error tht cannot be retried.
    ///
    /// Return `Ok(ControlFlow::Continue(res))` for retryable error, `res`
    /// will contain the previous `Result<Response, ReqwestError>`.
    /// A retryable error could be a `ReqwestError` or `Response` with
    /// unsuccessful status code.
    ///
    /// Return `Ok(ControlFlow::Break(response))` when succeeds and no need
    /// to retry.
    async fn do_send_request(
        &self,
        method: &Method,
        url: &Url,
    ) -> Result<ControlFlow<Response, Result<Response, ReqwestError>>, ReqwestError> {
        let request = Request::new(method.clone(), url.clone());

        let future = (&self.0.service).ready().await?.call(request);

        let response = match future.await {
            Err(err) if err.is_timeout() => {
                // Delay further request on timeout
                self.0
                    .service
                    .add_urls_to_delay(&[url], RETRY_DURATION_FOR_TIMEOUT);

                return Ok(ControlFlow::Continue(Err(err)));
            }
            res => res?,
        };

        let status = response.status();

        let add_delay_and_continue = |response: Response, duration| {
            info!("Receiver status code {status}, will wait for {duration:#?} and retry");

            self.0
                .service
                .add_urls_to_delay(&[url, response.url()], duration);

            Ok(ControlFlow::Continue(Ok(response)))
        };

        match status {
            // Delay further request on rate limit
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => {
                let duration = parse_header_retry_after(response.headers())
                    .unwrap_or(DEFAULT_RETRY_DURATION_FOR_RATE_LIMIT);

                let duration = duration.min(MAX_RETRY_DURATION);

                add_delay_and_continue(response, duration)
            }

            // Delay further request on timeout
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
                add_delay_and_continue(response, RETRY_DURATION_FOR_TIMEOUT)
            }

            _ => Ok(ControlFlow::Break(response)),
        }
    }

    async fn send_request_inner(
        &self,
        method: &Method,
        url: &Url,
    ) -> Result<Response, ReqwestError> {
        let mut count = 0;
        let max_retry_count = NonZeroU8::new(MAX_RETRY_COUNT).unwrap();

        // Since max_retry_count is non-zero, there is at least one iteration.
        loop {
            // Increment the counter before checking for terminal condition.
            count += 1;

            match self.do_send_request(method, url).await? {
                ControlFlow::Break(response) => break Ok(response),
                ControlFlow::Continue(res) if count >= max_retry_count.get() => {
                    break res;
                }
                _ => (),
            }
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

    async fn head_or_fallback_to_get(&self, url: Url) -> Result<Response, Error> {
        let res = self.send_request(Method::HEAD, url.clone(), true).await;

        match res {
            Err(Error::Http(http_error))
                if http_error.err.status() == Some(StatusCode::METHOD_NOT_ALLOWED) =>
            {
                // Retry using GET
                self.send_request(Method::GET, url, true).await
            }
            res => res,
        }
    }

    /// Check if remote exists using `Method::HEAD` or `Method::GET` as fallback.
    pub async fn remote_gettable(&self, url: Url) -> Result<bool, Error> {
        self.head_or_fallback_to_get(url)
            .await
            .map(|response| response.status().is_success())
    }

    /// Attempt to get final redirected url using `Method::HEAD` or fallback
    /// to `Method::GET`.
    pub async fn get_redirected_final_url(&self, url: Url) -> Result<Url, Error> {
        self.head_or_fallback_to_get(url)
            .await
            .map(|response| response.url().clone())
    }

    /// Create `GET` request to `url` and return a stream of the response data.
    /// On status code other than 200, it will return an error.
    pub async fn get_stream(
        &self,
        url: Url,
    ) -> Result<impl Stream<Item = Result<Bytes, Error>>, Error> {
        debug!("Downloading from: '{url}'");

        let response = self.send_request(Method::GET, url.clone(), true).await?;

        let url = Box::new(url);

        Ok(response.bytes_stream().map(move |res| {
            res.map_err(|err| {
                Error::Http(Box::new(HttpError {
                    method: Method::GET,
                    url: Url::clone(&*url),
                    err,
                }))
            })
        }))
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
