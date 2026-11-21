//! Minimal `Alt-Svc` ([RFC 7838]) parser, only extracting what is needed to
//! decide whether an origin can be reached over HTTP/3.
//!
//! [RFC 7838]: https://www.rfc-editor.org/rfc/rfc7838

use std::time::Duration;

/// Freshness lifetime used when an alternative has no `ma` parameter.
const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// What one or more `Alt-Svc` header values say about HTTP/3 for an origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AltSvc {
    /// The origin offers `h3` on its own host and port for `max_age`.
    H3 { max_age: Duration },
    /// The origin advertises no usable `h3` alternative (or sent `clear`),
    /// which replaces whatever was advertised before.
    None,
}

/// Parse all `Alt-Svc` values of one response.
///
/// Only an `h3` alternative pointing at the origin itself is usable, since
/// reqwest's HTTP/3 client connects to the host and port of the request URL.
pub(super) fn parse<'a>(
    values: impl IntoIterator<Item = &'a str>,
    origin_host: &str,
    origin_port: u16,
) -> AltSvc {
    let mut max_age = None;

    for value in values {
        for alternative in split_unquoted(value, ',') {
            let mut parts = split_unquoted(alternative, ';').map(str::trim);

            let Some((protocol, authority)) = parts.next().and_then(|p| p.split_once('=')) else {
                // `clear`, or garbage: neither gives an h3 alternative.
                continue;
            };

            if protocol.trim() != "h3" {
                continue;
            }

            let Some(authority) = unquote(authority.trim()) else {
                continue;
            };
            let Some((host, port)) = authority.rsplit_once(':') else {
                continue;
            };
            if !(host.is_empty() || host.eq_ignore_ascii_case(origin_host))
                || port.parse::<u16>().ok() != Some(origin_port)
            {
                continue;
            }

            let ma = parts
                .filter_map(|param| param.split_once('='))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("ma"))
                .map(|(_, ma)| {
                    unquote(ma.trim())
                        .and_then(|ma| ma.parse().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(Duration::ZERO)
                })
                .unwrap_or(DEFAULT_MAX_AGE);

            max_age = max_age.max(Some(ma));
        }
    }

    match max_age {
        Some(max_age) if !max_age.is_zero() => AltSvc::H3 { max_age },
        _ => AltSvc::None,
    }
}

/// Split `s` on `sep`, ignoring separators inside double-quoted strings.
fn split_unquoted(s: &str, sep: char) -> impl Iterator<Item = &str> {
    let mut rest = Some(s);

    std::iter::from_fn(move || {
        let s = rest?;
        let mut quoted = false;
        let mut escaped = false;

        for (i, c) in s.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' if quoted => escaped = true,
                '"' => quoted = !quoted,
                c if c == sep && !quoted => {
                    rest = Some(&s[i + 1..]);
                    return Some(&s[..i]);
                }
                _ => (),
            }
        }

        rest = None;
        Some(s)
    })
}

/// Strip optional surrounding double quotes (alt-authority is always quoted,
/// parameter values may be either a token or a quoted string).
fn unquote(s: &str) -> Option<&str> {
    match s.strip_prefix('"') {
        Some(s) => s.strip_suffix('"'),
        None => Some(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h3(secs: u64) -> AltSvc {
        AltSvc::H3 {
            max_age: Duration::from_secs(secs),
        }
    }

    fn parse_one(value: &str) -> AltSvc {
        parse([value], "example.com", 443)
    }

    #[test]
    fn same_origin_h3() {
        assert_eq!(parse_one(r#"h3=":443"; ma=86400"#), h3(86400));
        assert_eq!(
            parse_one(r#"h3=":443""#),
            AltSvc::H3 {
                max_age: DEFAULT_MAX_AGE
            }
        );
        assert_eq!(parse_one(r#"h3="example.com:443";ma="60""#), h3(60));
        assert_eq!(
            parse_one(r#"h3="EXAMPLE.com:443"; persist=1; ma=60"#),
            h3(60)
        );
    }

    #[test]
    fn picks_h3_among_alternatives() {
        // Cloudflare style.
        assert_eq!(
            parse_one(r#"h3-29=":443"; ma=93600, h3=":443"; ma=86400, h2=":443""#),
            h3(86400)
        );
        // Spread over several header values, longest freshness wins.
        assert_eq!(
            parse(
                [r#"h2=":443""#, r#"h3=":443"; ma=10"#, r#"h3=":443"; ma=20"#],
                "example.com",
                443
            ),
            h3(20)
        );
    }

    #[test]
    fn unusable_alternatives() {
        assert_eq!(parse_one("clear"), AltSvc::None);
        assert_eq!(parse_one(r#"h2=":443""#), AltSvc::None);
        assert_eq!(parse_one(r#"h3-29=":443""#), AltSvc::None);
        // Different port or host: reqwest can only dial the URL's authority.
        assert_eq!(parse_one(r#"h3=":8443""#), AltSvc::None);
        assert_eq!(parse_one(r#"h3="alt.example.net:443""#), AltSvc::None);
        // Expired immediately.
        assert_eq!(parse_one(r#"h3=":443"; ma=0"#), AltSvc::None);
        // Malformed.
        assert_eq!(parse_one(r#"h3=":443"; ma=soon"#), AltSvc::None);
        assert_eq!(parse_one(r#"h3=""#), AltSvc::None);
        assert_eq!(parse_one(""), AltSvc::None);
    }

    #[test]
    fn quoted_separators_do_not_split() {
        assert_eq!(
            split_unquoted(r#"a="x,y", b"#, ',').collect::<Vec<_>>(),
            [r#"a="x,y""#, " b"]
        );
        assert_eq!(
            split_unquoted(r#"a="x\";y"; c"#, ';').collect::<Vec<_>>(),
            [r#"a="x\";y""#, " c"]
        );
    }
}
