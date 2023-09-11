use std::{
    num::{NonZeroU16, NonZeroU64, NonZeroU8},
    ops::ControlFlow,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use futures_util::Stream;
use httpdate::parse_http_date;
use reqwest::{
    header::{HeaderMap, RETRY_AFTER},
    Request,
};
use thiserror::Error as ThisError;
use tracing::{debug, info, instrument};

pub use reqwest::{header, Error as ReqwestError, Method, StatusCode};
pub use url::Url;

#[cfg(feature = "trust-dns")]
use crate::resolver::DefaultResolver;

mod delay_request;
use delay_request::DelayRequest;

mod certificate;
pub use certificate::Certificate;

mod request_builder;
pub use request_builder::{Body, RequestBuilder, Response};

mod tls_version;
pub use tls_version::TLSVersion;

#[cfg(feature = "json")]
pub use request_builder::JsonError;

const MAX_RETRY_DURATION: Duration = Duration::from_secs(120);
const MAX_RETRY_COUNT: u8 = 3;
const DEFAULT_RETRY_DURATION_FOR_RATE_LIMIT: Duration = Duration::from_millis(200);
const RETRY_DURATION_FOR_TIMEOUT: Duration = Duration::from_millis(200);
#[allow(dead_code)]
const DEFAULT_MIN_TLS: TLSVersion = TLSVersion::TLS_1_2;

#[derive(Debug, ThisError)]
#[non_exhaustive]
pub enum Error {
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error(transparent)]
    Http(Box<HttpError>),

    #[cfg(feature = "json")]
    #[error("Failed to parse http response body as Json: {0}")]
    Json(#[from] JsonError),
}

#[derive(Debug, ThisError)]
#[error("could not {method} {url}: {err}")]
pub struct HttpError {
    method: reqwest::Method,
    url: url::Url,
    #[source]
    err: reqwest::Error,
}

impl HttpError {
    /// Returns true if the error is from [`Response::error_for_status`].
    pub fn is_status(&self) -> bool {
        self.err.is_status()
    }
}

#[derive(Debug)]
struct Inner {
    client: reqwest::Client,
    service: DelayRequest,
}

#[derive(Clone, Debug)]
pub struct Client(Arc<Inner>);

#[cfg_attr(not(feature = "__tls"), allow(unused_variables, unused_mut))]
impl Client {
    /// * `per_millis` - The duration (in millisecond) for which at most
    ///   `num_request` can be sent, itcould be increased if rate-limit
    ///   happens.
    /// * `num_request` - maximum number of requests to be processed for
    ///   each `per` duration.
    ///
    /// The Client created would use at least tls 1.2
    pub fn new(
        user_agent: impl AsRef<str>,
        min_tls: Option<TLSVersion>,
        per_millis: NonZeroU16,
        num_request: NonZeroU64,
        certificates: impl IntoIterator<Item = Certificate>,
    ) -> Result<Self, Error> {
        fn inner(
            user_agent: &str,
            min_tls: Option<TLSVersion>,
            per_millis: NonZeroU16,
            num_request: NonZeroU64,
            certificates: &mut dyn Iterator<Item = Certificate>,
        ) -> Result<Client, Error> {
            let mut builder = reqwest::ClientBuilder::new()
                .user_agent(user_agent)
                .https_only(true)
                .tcp_nodelay(false);

            #[cfg(feature = "trust-dns")]
            {
                builder = builder.dns_resolver(Arc::new(DefaultResolver::default()));
            }

            #[cfg(feature = "__tls")]
            {
                let tls_ver = min_tls
                    .map(|tls| tls.max(DEFAULT_MIN_TLS))
                    .unwrap_or(DEFAULT_MIN_TLS);

                builder = builder.min_tls_version(tls_ver.into());

                for certificate in certificates {
                    builder = builder.add_root_certificate(certificate.0);
                }
            }

            let client = builder.build()?;

            Ok(Client(Arc::new(Inner {
                client: client.clone(),
                service: DelayRequest::new(
                    num_request,
                    Duration::from_millis(per_millis.get() as u64),
                    client,
                ),
            })))
        }

        inner(
            user_agent.as_ref(),
            min_tls,
            per_millis,
            num_request,
            &mut certificates.into_iter(),
        )
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
    #[instrument]
    async fn do_send_request(
        &self,
        request: Request,
        url: &Url,
    ) -> Result<ControlFlow<reqwest::Response, Result<reqwest::Response, ReqwestError>>, ReqwestError>
    {
        let response = match self.0.service.call(request).await {
            Err(err) if err.is_timeout() || err.is_connect() => {
                let duration = RETRY_DURATION_FOR_TIMEOUT;

                info!("Received timeout error from reqwest. Delay future request by {duration:#?}");

                self.0.service.add_urls_to_delay(&[url], duration);

                return Ok(ControlFlow::Continue(Err(err)));
            }
            res => res?,
        };

        let status = response.status();

        let add_delay_and_continue = |response: reqwest::Response, duration| {
            info!("Received status code {status}, will wait for {duration:#?} and retry");

            self.0
                .service
                .add_urls_to_delay(&[url, response.url()], duration);

            Ok(ControlFlow::Continue(Ok(response)))
        };

        match status {
            // Delay further request on rate limit
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => {
                let duration = parse_header_retry_after(response.headers())
                    .unwrap_or(DEFAULT_RETRY_DURATION_FOR_RATE_LIMIT)
                    .min(MAX_RETRY_DURATION);

                add_delay_and_continue(response, duration)
            }

            // Delay further request on timeout
            StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
                add_delay_and_continue(response, RETRY_DURATION_FOR_TIMEOUT)
            }

            _ => Ok(ControlFlow::Break(response)),
        }
    }

    /// * `request` - `Request::try_clone` must always return `Some`.
    async fn send_request_inner(
        &self,
        request: &Request,
    ) -> Result<reqwest::Response, ReqwestError> {
        let mut count = 0;
        let max_retry_count = NonZeroU8::new(MAX_RETRY_COUNT).unwrap();

        // Since max_retry_count is non-zero, there is at least one iteration.
        loop {
            // Increment the counter before checking for terminal condition.
            count += 1;

            match self
                .do_send_request(request.try_clone().unwrap(), request.url())
                .await?
            {
                ControlFlow::Break(response) => break Ok(response),
                ControlFlow::Continue(res) if count >= max_retry_count.get() => {
                    break res;
                }
                _ => (),
            }
        }
    }

    /// * `request` - `Request::try_clone` must always return `Some`.
    async fn send_request(
        &self,
        request: Request,
        error_for_status: bool,
    ) -> Result<reqwest::Response, Error> {
        debug!("Downloading from: '{}'", request.url());

        self.send_request_inner(&request)
            .await
            .and_then(|response| {
                if error_for_status {
                    response.error_for_status()
                } else {
                    Ok(response)
                }
            })
            .map_err(|err| {
                Error::Http(Box::new(HttpError {
                    method: request.method().clone(),
                    url: request.url().clone(),
                    err,
                }))
            })
    }

    async fn head_or_fallback_to_get(
        &self,
        url: Url,
        error_for_status: bool,
    ) -> Result<reqwest::Response, Error> {
        let res = self
            .send_request(Request::new(Method::HEAD, url.clone()), error_for_status)
            .await;

        let retry_with_get = move || async move {
            // Retry using GET
            info!("HEAD on {url} is not allowed, fallback to GET");
            self.send_request(Request::new(Method::GET, url), error_for_status)
                .await
        };

        let is_retryable = |status| {
            matches!(
                status,
                StatusCode::BAD_REQUEST              // 400
                    | StatusCode::UNAUTHORIZED       // 401
                    | StatusCode::FORBIDDEN          // 403
                    | StatusCode::NOT_FOUND          // 404
                    | StatusCode::METHOD_NOT_ALLOWED // 405
                    | StatusCode::GONE // 410
            )
        };

        match res {
            Err(Error::Http(http_error))
                if http_error.err.status().map(is_retryable).unwrap_or(false) =>
            {
                retry_with_get().await
            }
            Ok(response) if is_retryable(response.status()) => retry_with_get().await,
            res => res,
        }
    }

    /// Check if remote exists using `Method::GET`.
    pub async fn remote_gettable(&self, url: Url) -> Result<bool, Error> {
        Ok(self.get(url).send(false).await?.status().is_success())
    }

    /// Attempt to get final redirected url using `Method::HEAD` or fallback
    /// to `Method::GET`.
    pub async fn get_redirected_final_url(&self, url: Url) -> Result<Url, Error> {
        self.head_or_fallback_to_get(url, true)
            .await
            .map(|response| response.url().clone())
    }

    /// Create `GET` request to `url` and return a stream of the response data.
    /// On status code other than 200, it will return an error.
    pub async fn get_stream(
        &self,
        url: Url,
    ) -> Result<impl Stream<Item = Result<Bytes, Error>>, Error> {
        Ok(self.get(url).send(true).await?.bytes_stream())
    }

    /// Create a new request.
    pub fn request(&self, method: Method, url: Url) -> RequestBuilder {
        RequestBuilder {
            client: self.clone(),
            inner: self.0.client.request(method, url),
        }
    }

    /// Create a new GET request.
    pub fn get(&self, url: Url) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    /// Create a new POST request.
    pub fn post(&self, url: Url, body: impl Into<Body>) -> RequestBuilder {
        self.request(Method::POST, url).body(body.into())
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
