use url::Url;

use super::Error;

/// Extension methods for [`reqwest::ClientBuilder`].
pub trait ClientBuilderExt: Sized {
    /// Sends every request through `proxy`, a proxy in the format of Cargo's
    /// `http.proxy` setting, which is libcurl's:
    /// `[scheme://][user:password@]host[:port]`.
    ///
    /// As with Cargo, `proxy` takes precedence over the proxy reqwest takes
    /// from the `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` environment
    /// variables and the system settings, while hosts in `NO_PROXY` are still
    /// reached directly. An empty `proxy` leaves the builder unchanged, so
    /// reqwest's default proxies still apply.
    fn cargo_http_proxy(self, proxy: &str) -> Result<Self, Error>;
}

impl ClientBuilderExt for reqwest::ClientBuilder {
    fn cargo_http_proxy(self, proxy: &str) -> Result<Self, Error> {
        if proxy.is_empty() {
            return Ok(self);
        }

        let proxy =
            reqwest::Proxy::all(libcurl_proxy_url(proxy)?)?.no_proxy(reqwest::NoProxy::from_env());

        // `ClientBuilder::proxy` also turns off reqwest's default proxies,
        // which reqwest has no public API to add back. They would never be
        // reached anyway: they are only tried for a request no added proxy
        // takes, and this one takes every request except to hosts in
        // `NO_PROXY`, which reqwest's defaults leave out too.
        Ok(self.proxy(proxy))
    }
}

/// Port libcurl uses for a proxy with no port given, unless it is an
/// `https://` proxy, which uses 443 as URLs do.
const LIBCURL_DEFAULT_PROXY_PORT: u16 = 1080;

/// Parses a proxy in libcurl's format: libcurl assumes `http://` without a
/// scheme, and port 1080 without a port, while reqwest would use port 80.
fn libcurl_proxy_url(proxy: &str) -> Result<Url, url::ParseError> {
    let proxy = if proxy.contains("://") {
        proxy.into()
    } else {
        format!("http://{proxy}")
    };
    let mut url = Url::parse(&proxy)?;

    // `Url` drops a port that is the scheme's default, so `http://host:80`
    // and `http://host` both have no port. Parse it again with a scheme
    // `Url` knows no default port for, to see whether one was given.
    if url.scheme() == "http" && Url::parse(&format!("libcurl+{proxy}"))?.port().is_none() {
        url.set_port(Some(LIBCURL_DEFAULT_PROXY_PORT))
            .expect("an http URL has a host, so it can have a port");
    }

    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libcurl_proxy_url_matches_libcurl_defaults() {
        for (proxy, expected) in [
            ("proxy.example", "http://proxy.example:1080/"),
            ("proxy.example:3128", "http://proxy.example:3128/"),
            ("http://proxy.example", "http://proxy.example:1080/"),
            ("HTTP://proxy.example/", "http://proxy.example:1080/"),
            ("http://proxy.example:80", "http://proxy.example/"),
            (
                "user:pass@proxy.example",
                "http://user:pass@proxy.example:1080/",
            ),
            (
                "http://user:p%40ss@proxy.example:8080",
                "http://user:p%40ss@proxy.example:8080/",
            ),
            ("[::1]", "http://[::1]:1080/"),
            ("[::1]:3128", "http://[::1]:3128/"),
            ("https://proxy.example", "https://proxy.example/"),
            ("socks5h://proxy.example", "socks5h://proxy.example"),
        ] {
            assert_eq!(
                libcurl_proxy_url(proxy).unwrap().as_str(),
                expected,
                "{proxy}"
            );
        }

        // `http://proxy.example:80` keeps port 80, `Url` just doesn't print it.
        assert_eq!(
            libcurl_proxy_url("http://proxy.example:80")
                .unwrap()
                .port_or_known_default(),
            Some(80)
        );
    }

    #[test]
    fn cargo_http_proxy_accepts_libcurl_formats() {
        for proxy in [
            "",
            "proxy.example",
            "user:pass@proxy.example:3128",
            "https://proxy.example",
            "socks5://proxy.example:1080",
        ] {
            reqwest::ClientBuilder::new()
                .cargo_http_proxy(proxy)
                .unwrap()
                .build()
                .unwrap();
        }
    }

    #[test]
    fn cargo_http_proxy_rejects_invalid_proxy() {
        assert!(
            reqwest::ClientBuilder::new()
                .cargo_http_proxy("http://[::1")
                .is_err()
        );
    }
}
