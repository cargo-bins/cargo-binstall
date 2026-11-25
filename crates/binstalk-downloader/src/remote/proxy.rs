use std::borrow::Cow;

use super::Error;

/// Sends every request of `builder` through `proxy`, a proxy in the format of
/// Cargo's `http.proxy` setting, which is libcurl's:
/// `[scheme://][user:password@]host[:port]`.
///
/// As with Cargo, this replaces the proxy otherwise taken from the
/// `HTTPS_PROXY`, `HTTP_PROXY` and `ALL_PROXY` environment variables and
/// the system settings, while hosts in `NO_PROXY` are still reached
/// directly. An empty `proxy` turns proxies off altogether.
pub fn set_cargo_http_proxy(
    builder: reqwest::ClientBuilder,
    proxy: &str,
) -> Result<reqwest::ClientBuilder, Error> {
    if proxy.is_empty() {
        return Ok(builder.no_proxy());
    }

    let proxy =
        reqwest::Proxy::all(&*libcurl_proxy_url(proxy))?.no_proxy(reqwest::NoProxy::from_env());

    Ok(builder.proxy(proxy))
}

/// Port libcurl uses for a proxy with no port given, unless it is an
/// `https://` proxy, which uses 443 as URLs do.
const LIBCURL_DEFAULT_PROXY_PORT: &str = "1080";

/// Converts a proxy in libcurl's format to a URL for reqwest: libcurl
/// assumes `http://` without a scheme and port 1080 without a port, while
/// reqwest would use port 80.
fn libcurl_proxy_url(proxy: &str) -> Cow<'_, str> {
    let (scheme, rest) = match proxy.split_once("://") {
        Some((scheme, rest)) => (Some(scheme), rest),
        None => (None, proxy),
    };

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let has_port = match host_port.strip_prefix('[') {
        // IPv6 address, like `[::1]:3128`
        Some(ipv6) => ipv6.contains("]:"),
        None => host_port.contains(':'),
    };

    let is_http = scheme.is_none_or(|scheme| scheme.eq_ignore_ascii_case("http"));

    match (scheme, is_http && !has_port) {
        (Some(_), false) => Cow::Borrowed(proxy),
        (None, false) => Cow::Owned(format!("http://{proxy}")),
        (_, true) => {
            let scheme = scheme.unwrap_or("http");
            let path = &rest[authority_end..];
            Cow::Owned(format!(
                "{scheme}://{authority}:{LIBCURL_DEFAULT_PROXY_PORT}{path}"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libcurl_proxy_url_matches_libcurl_defaults() {
        for (proxy, expected) in [
            ("proxy.example", "http://proxy.example:1080"),
            ("proxy.example:3128", "http://proxy.example:3128"),
            ("http://proxy.example", "http://proxy.example:1080"),
            ("HTTP://proxy.example/", "HTTP://proxy.example:1080/"),
            ("http://proxy.example:80", "http://proxy.example:80"),
            (
                "user:pass@proxy.example",
                "http://user:pass@proxy.example:1080",
            ),
            (
                "http://user:p%40ss@proxy.example:8080",
                "http://user:p%40ss@proxy.example:8080",
            ),
            ("[::1]", "http://[::1]:1080"),
            ("[::1]:3128", "http://[::1]:3128"),
            ("https://proxy.example", "https://proxy.example"),
            ("socks5h://proxy.example", "socks5h://proxy.example"),
        ] {
            assert_eq!(libcurl_proxy_url(proxy), expected, "{proxy}");
        }
    }

    #[test]
    fn set_cargo_http_proxy_accepts_libcurl_formats() {
        for proxy in [
            "",
            "proxy.example",
            "user:pass@proxy.example:3128",
            "https://proxy.example",
            "socks5://proxy.example:1080",
        ] {
            set_cargo_http_proxy(reqwest::ClientBuilder::new(), proxy)
                .unwrap()
                .build()
                .unwrap();
        }
    }
}
