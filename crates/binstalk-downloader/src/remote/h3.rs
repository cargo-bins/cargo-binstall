//! Opportunistic HTTP/3.
//!
//! reqwest only speaks HTTP/3 to a request explicitly marked as such and has
//! neither `Alt-Svc` discovery nor a fallback to TCP, so this module adds both:
//!
//! - Every response's `Alt-Svc` is recorded, so later requests to an origin
//!   that advertised `h3` are sent over HTTP/3.
//! - The HTTP/3 attempt gets a short head start, after which the same request
//!   also goes out over TCP and the first success wins, so a network that
//!   drops UDP costs at most [`HEAD_START`]. An origin whose HTTP/3 failed or
//!   lost the race is not tried again for a while, with exponential backoff.
//! - Requests that would go through a proxy stay on TCP, since reqwest's
//!   HTTP/3 client does not support proxies.
//! - Redirects are only followed over HTTP/3 to origins known to support it,
//!   otherwise the whole request is sent again over TCP.

use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, UdpSocket},
    pin::pin,
    sync::{Arc, Mutex},
};

use compact_str::{CompactString, ToCompactString};
use hyper_util::client::proxy::matcher::Matcher;
use reqwest::{
    Method, Request, Response, Version,
    header::{ALT_SVC, HeaderMap, LOCATION},
    redirect,
};
use tokio::time::{Duration, Instant, sleep};
use tracing::debug;
use url::Url;

use super::delay_request::{DelayRequest, RequestResult};

mod alt_svc;
use alt_svc::AltSvc;

/// How long HTTP/3 runs alone before the request is also sent over TCP.
const HEAD_START: Duration = Duration::from_millis(250);
/// How long an origin is kept off HTTP/3 after its first failure, doubled for
/// every consecutive failure up to `BROKEN_BASE << MAX_BROKEN_SHIFT`.
const BROKEN_BASE: Duration = Duration::from_secs(5 * 60);
const MAX_BROKEN_SHIFT: u32 = 6;
/// Same as reqwest's default redirect policy.
const MAX_REDIRECTS: usize = 10;

#[derive(Debug, Default)]
struct OriginState {
    /// `h3` was advertised for this origin, fresh until then.
    h3_until: Option<Instant>,
    /// HTTP/3 to this origin recently failed, don't use it until then.
    broken_until: Option<Instant>,
    consecutive_failures: u32,
}

/// HTTP/3 knowledge per origin (host, port); only https origins are tracked.
#[derive(Debug, Default)]
struct Origins(Mutex<HashMap<(CompactString, u16), OriginState>>);

fn origin(url: &Url) -> Option<(CompactString, u16)> {
    (url.scheme() == "https")
        .then(|| {
            Some((
                url.host_str()?.to_compact_string(),
                url.port_or_known_default()?,
            ))
        })
        .flatten()
}

impl Origins {
    fn supports_h3(&self, url: &Url) -> bool {
        let Some(origin) = origin(url) else {
            return false;
        };
        let now = Instant::now();

        self.0.lock().unwrap().get(&origin).is_some_and(|state| {
            state.h3_until.is_some_and(|until| until > now)
                && state.broken_until.is_none_or(|until| until <= now)
        })
    }

    fn learn(&self, url: &Url, headers: &HeaderMap) {
        let Some(origin) = origin(url) else {
            return;
        };
        let mut values = headers
            .get_all(ALT_SVC)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .peekable();
        if values.peek().is_none() {
            return;
        }

        let alt_svc = alt_svc::parse(values, &origin.0, origin.1);
        let mut origins = self.0.lock().unwrap();
        match alt_svc {
            AltSvc::H3 { max_age } => {
                origins.entry(origin).or_default().h3_until = Some(Instant::now() + max_age)
            }
            AltSvc::None => {
                if let Some(state) = origins.get_mut(&origin) {
                    state.h3_until = None;
                }
            }
        }
    }

    fn mark_broken(&self, url: &Url) {
        let Some(origin) = origin(url) else {
            return;
        };
        let mut origins = self.0.lock().unwrap();
        let state = origins.entry(origin).or_default();

        let backoff = BROKEN_BASE * (1 << state.consecutive_failures.min(MAX_BROKEN_SHIFT));
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.broken_until = Some(Instant::now() + backoff);
    }

    fn mark_working(&self, url: &Url) {
        let Some(origin) = origin(url) else {
            return;
        };
        if let Some(state) = self.0.lock().unwrap().get_mut(&origin) {
            state.consecutive_failures = 0;
            state.broken_until = None;
        }
    }
}

pub(super) struct H3 {
    client: reqwest::Client,
    origins: Arc<Origins>,
    proxy: Matcher,
}

impl fmt::Debug for H3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("H3")
            .field("client", &self.client)
            .field("origins", &self.origins)
            .finish_non_exhaustive()
    }
}

impl H3 {
    /// `builder` should be configured like the TCP client (user agent, TLS,
    /// DNS resolver, ...).
    pub(super) fn new(builder: reqwest::ClientBuilder) -> Result<Self, reqwest::Error> {
        let origins = Arc::<Origins>::default();

        let redirect_policy = {
            let origins = origins.clone();
            redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    attempt.error("too many redirects")
                } else if origins.supports_h3(attempt.url()) {
                    attempt.follow()
                } else {
                    // Hand the redirect back, see `H3::outcome`.
                    attempt.stop()
                }
            })
        };

        let mut builder = builder
            .http3_prior_knowledge()
            .http3_congestion_bbr()
            .tls_early_data(true)
            .redirect(redirect_policy);

        // reqwest binds the QUIC endpoint to `[::]:0`, which fails outright
        // where IPv6 is disabled.
        if UdpSocket::bind((Ipv6Addr::UNSPECIFIED, 0)).is_err() {
            builder = builder.local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        }

        Ok(Self {
            client: builder.build()?,
            origins,
            proxy: Matcher::from_system(),
        })
    }

    /// Record the `Alt-Svc` advertised by `response`.
    pub(super) fn learn(&self, response: &Response) {
        self.origins.learn(response.url(), response.headers())
    }

    /// The body of an HTTP/3 response from `url` broke off.
    pub(super) fn body_broke(&self, url: &Url) {
        debug!("HTTP/3 body from {url} broke off, not using HTTP/3 for it for a while");
        self.origins.mark_broken(url);
    }

    /// Whether `request` should be tried over HTTP/3.
    pub(super) fn should_try(&self, request: &Request) -> bool {
        // Racing sends the request twice, only do that when it is idempotent.
        matches!(*request.method(), Method::GET | Method::HEAD)
            && self.origins.supports_h3(request.url())
            && !self.is_proxied(request.url())
    }

    fn is_proxied(&self, url: &Url) -> bool {
        url.as_str()
            .parse()
            .map_or(true, |uri| self.proxy.intercept(&uri).is_some())
    }

    /// Send `request` over HTTP/3, racing it against `tcp` once
    /// [`HEAD_START`] has passed.
    pub(super) async fn race(
        &self,
        service: &DelayRequest,
        tcp: &reqwest::Client,
        request: Request,
    ) -> RequestResult {
        let Some(tcp_request) = request.try_clone() else {
            return service.call(tcp, request).await;
        };
        let url = request.url().clone();

        let mut h3_request = request;
        *h3_request.version_mut() = Version::HTTP_3;
        let mut h3 = pin!(service.call(&self.client, h3_request));

        let res = tokio::select! {
            biased;
            res = &mut h3 => Some(res),
            () = sleep(HEAD_START) => None,
        };
        if let Some(res) = res {
            return match self.outcome(&url, res) {
                Some(response) => Ok(response),
                None => service.call(tcp, tcp_request).await,
            };
        }

        debug!("HTTP/3 to {url} is taking a while, racing it against TCP");
        let mut tcp = pin!(service.call(tcp, tcp_request));

        tokio::select! {
            biased;
            res = &mut h3 => match self.outcome(&url, res) {
                Some(response) => Ok(response),
                None => tcp.await,
            },
            res = &mut tcp => match res {
                Ok(response) => {
                    debug!("TCP beat HTTP/3 to {url}");
                    self.origins.mark_broken(&url);
                    Ok(response)
                }
                Err(err) => self.outcome(&url, h3.await).ok_or(err),
            },
        }
    }

    /// The response to use, or `None` to (keep) using TCP instead.
    fn outcome(&self, url: &Url, res: RequestResult) -> Option<Response> {
        match res {
            Ok(response)
                if response.status().is_redirection()
                    && response.headers().contains_key(LOCATION) =>
            {
                debug!(
                    "{url} redirects to {} without known HTTP/3 support, retrying over TCP",
                    response.headers()[LOCATION]
                        .to_str()
                        .unwrap_or("<non-ascii>"),
                );
                None
            }
            Ok(response) => {
                debug!("Used HTTP/3 for {url}");
                self.origins.mark_working(url);
                Some(response)
            }
            Err(err) => {
                debug!("HTTP/3 to {url} failed, falling back to TCP: {err}");
                self.origins.mark_broken(err.url().unwrap_or(url));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use reqwest::header::HeaderValue;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn alt_svc(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ALT_SVC, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn origins_learn_and_forget() {
        let origins = Origins::default();
        let u = url("https://example.com/a");

        assert!(!origins.supports_h3(&u));

        // No Alt-Svc header: nothing changes.
        origins.learn(&u, &HeaderMap::new());
        assert!(!origins.supports_h3(&u));

        origins.learn(&u, &alt_svc(r#"h3=":443"; ma=60"#));
        assert!(origins.supports_h3(&u));
        assert!(origins.supports_h3(&url("https://example.com:443/other/path")));
        assert!(!origins.supports_h3(&url("https://example.com:8443/")));
        assert!(!origins.supports_h3(&url("https://www.example.com/")));
        // Never for plaintext.
        assert!(!origins.supports_h3(&url("http://example.com:443/")));

        // An Alt-Svc without h3 replaces the earlier one.
        origins.learn(&u, &alt_svc(r#"h2=":443""#));
        assert!(!origins.supports_h3(&u));

        origins.learn(&u, &alt_svc(r#"h3=":443""#));
        assert!(origins.supports_h3(&u));
        origins.learn(&u, &alt_svc("clear"));
        assert!(!origins.supports_h3(&u));
    }

    #[test]
    fn plaintext_is_never_learnt() {
        let origins = Origins::default();
        let u = url("http://example.com:443/");
        origins.learn(&u, &alt_svc(r#"h3=":443""#));
        assert!(origins.0.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn broken_backs_off_exponentially() {
        let origins = Origins::default();
        let u = url("https://example.com/");
        origins.learn(&u, &alt_svc(r#"h3=":443"; ma=31536000"#));

        for shift in 0..=MAX_BROKEN_SHIFT + 1 {
            origins.mark_broken(&u);
            assert!(!origins.supports_h3(&u));

            let backoff = BROKEN_BASE * (1 << shift.min(MAX_BROKEN_SHIFT));
            tokio::time::advance(backoff - Duration::from_secs(1)).await;
            assert!(!origins.supports_h3(&u), "still broken before {backoff:?}");
            tokio::time::advance(Duration::from_secs(1)).await;
            assert!(origins.supports_h3(&u), "usable again after {backoff:?}");
        }

        origins.mark_working(&u);
        origins.mark_broken(&u);
        tokio::time::advance(BROKEN_BASE).await;
        assert!(origins.supports_h3(&u), "success resets the backoff");
    }

    #[tokio::test(start_paused = true)]
    async fn alt_svc_expires() {
        let origins = Origins::default();
        let u = url("https://example.com/");
        origins.learn(&u, &alt_svc(r#"h3=":443"; ma=60"#));

        tokio::time::advance(Duration::from_secs(59)).await;
        assert!(origins.supports_h3(&u));
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(!origins.supports_h3(&u));
    }
}
