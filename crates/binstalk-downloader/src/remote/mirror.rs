use std::{fmt, str::FromStr};

use reqwest::{
    Request,
    header::{AUTHORIZATION, HeaderValue},
};
use thiserror::Error as ThisError;
use url::{Position, Url};

/// Sends the requests for urls under one prefix to another prefix instead,
/// e.g. `https://github.com/` to an Artifactory remote repository that
/// proxies it.
///
/// It is written as `FROM=TO`, e.g.
/// `https://github.com/=https://artifactory.example.com/artifactory/github/`.
/// `FROM` matches whole path segments, so `https://github.com/foo` matches
/// `https://github.com/foo/bar` but not `https://github.com/foobar`.
///
/// The `Authorization` header of a request sent to `TO` is dropped, since
/// it is meant for `FROM` (the same as reqwest does when it follows a
/// redirect to another host); credentials in `TO`
/// (`https://user:password@host/`) are sent to it as basic auth instead.
#[derive(Clone)]
pub struct Mirror {
    from: Url,
    to: Url,
    auth: Option<HeaderValue>,
}

#[derive(Debug, ThisError)]
#[non_exhaustive]
pub enum MirrorError {
    #[error("expected `FROM=TO`, e.g. `https://github.com/=https://mirror.example.com/github/`")]
    MissingEquals,

    #[error("invalid url `{url}`: {err}")]
    InvalidUrl {
        url: Box<str>,
        #[source]
        err: url::ParseError,
    },

    #[error("`{0}` must be an http(s) url without query or fragment")]
    NotAPrefix(Box<str>),

    #[error("`{0}` must not contain credentials, only the mirror may")]
    CredentialsInFrom(Box<str>),
}

impl Mirror {
    pub fn new(mut from: Url, mut to: Url) -> Result<Self, MirrorError> {
        for url in [&mut from, &mut to] {
            if !matches!(url.scheme(), "http" | "https")
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(MirrorError::NotAPrefix(redact(url).into()));
            }
            // So that the prefix ends at a path segment.
            if !url.path().ends_with('/') {
                url.set_path(&format!("{}/", url.path()));
            }
        }
        if from.has_authority() && (!from.username().is_empty() || from.password().is_some()) {
            return Err(MirrorError::CredentialsInFrom(redact(&from).into()));
        }

        Ok(Self {
            from,
            to,
            auth: None,
        })
    }

    /// Moves the credentials in `to` into a basic auth header, built by
    /// `client` (reqwest turns them into one).
    pub(super) fn extract_auth(&mut self, client: &reqwest::Client) {
        if self.to.username().is_empty() && self.to.password().is_none() {
            return;
        }
        if let Ok(request) = client.get(self.to.clone()).build() {
            self.auth = request.headers().get(AUTHORIZATION).cloned();
            self.to = request.url().clone();
        }
    }

    /// The path of `url` after the prefix `prefix`, if it is under it.
    fn strip_prefix<'u>(prefix: &Url, url: &'u Url) -> Option<&'u str> {
        // Scheme, credentials, host and port must be the same.
        if prefix[..Position::BeforePath] != url[..Position::BeforePath] {
            return None;
        }
        url.path().strip_prefix(prefix.path())
    }

    fn replace_prefix(url: &Url, rest: &str, new_prefix: &Url) -> Url {
        let mut new = new_prefix.clone();
        new.set_path(&format!("{}{rest}", new_prefix.path()));
        new.set_query(url.query());
        new.set_fragment(url.fragment());
        new
    }
}

impl FromStr for Mirror {
    type Err = MirrorError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (from, to) = s.split_once('=').ok_or(MirrorError::MissingEquals)?;
        let parse = |url: &str| {
            Url::parse(url).map_err(|err| MirrorError::InvalidUrl {
                url: url.into(),
                err,
            })
        };
        Self::new(parse(from)?, parse(to)?)
    }
}

/// The url with its password hidden.
fn redact(url: &Url) -> String {
    if url.password().is_some() {
        let mut url = url.clone();
        let _ = url.set_password(Some("***"));
        url.into()
    } else {
        url.as_str().to_owned()
    }
}

impl fmt::Display for Mirror {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={}", self.from, redact(&self.to))
    }
}

impl fmt::Debug for Mirror {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Finds the mirror with the longest `from` prefix of `url`.
fn find<'m>(
    mirrors: &'m [Mirror],
    url: &Url,
    prefix: impl Fn(&Mirror) -> &Url,
) -> Option<(&'m Mirror, usize)> {
    mirrors
        .iter()
        .filter_map(|mirror| {
            Mirror::strip_prefix(prefix(mirror), url)
                .map(|rest| (mirror, url.path().len() - rest.len()))
        })
        .max_by_key(|(_, matched)| *matched)
}

/// Sends `request` to its mirror, if there is one.
///
/// Returns whether it was rewritten.
pub(super) fn rewrite(mirrors: &[Mirror], request: &mut Request) -> bool {
    let Some((mirror, matched)) = find(mirrors, request.url(), |mirror| &mirror.from) else {
        return false;
    };
    let url = request.url();
    let new = Mirror::replace_prefix(url, &url.path()[matched..], &mirror.to);
    *request.url_mut() = new;

    let headers = request.headers_mut();
    headers.remove(AUTHORIZATION);
    if let Some(auth) = &mirror.auth {
        headers.insert(AUTHORIZATION, auth.clone());
    }

    true
}

/// Maps `url` under a mirror back to the url it mirrors, so that callers see
/// the urls they asked for, e.g. when following redirects.
pub(super) fn unmirror(mirrors: &[Mirror], url: &Url) -> Option<Url> {
    let (mirror, matched) = find(mirrors, url, |mirror| &mirror.to)?;
    Some(Mirror::replace_prefix(
        url,
        &url.path()[matched..],
        &mirror.from,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    use reqwest::Method;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn mirrors(specs: &[&str]) -> Vec<Mirror> {
        let client = reqwest::Client::builder().build().unwrap();
        specs
            .iter()
            .map(|spec| {
                let mut mirror: Mirror = spec.parse().unwrap();
                mirror.extract_auth(&client);
                mirror
            })
            .collect()
    }

    fn rewritten(mirrors: &[Mirror], from: &str) -> Option<String> {
        let mut request = Request::new(Method::GET, url(from));
        rewrite(mirrors, &mut request).then(|| request.url().to_string())
    }

    #[test]
    fn parse() {
        let mirror: Mirror = "https://github.com=https://mirror.example.com/github"
            .parse()
            .unwrap();
        assert_eq!(
            mirror.to_string(),
            "https://github.com/=https://mirror.example.com/github/"
        );

        for invalid in [
            "https://github.com/",
            "github.com=https://mirror.example.com/",
            "https://github.com/=mirror.example.com",
            "https://github.com/?a=1=https://mirror.example.com/",
            "https://github.com/=https://mirror.example.com/?a=1",
            "https://github.com/=https://mirror.example.com/#a",
            "ftp://github.com/=https://mirror.example.com/",
            "https://u:p@github.com/=https://mirror.example.com/",
        ] {
            assert!(invalid.parse::<Mirror>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn rewrites_urls_under_from() {
        let mirrors = mirrors(&[
            "https://github.com/=https://mirror.example.com/artifactory/github/",
            "https://api.github.com/=https://mirror.example.com/artifactory/github-api/",
        ]);

        assert_eq!(
            rewritten(
                &mirrors,
                "https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/a.tgz"
            )
            .as_deref(),
            Some(
                "https://mirror.example.com/artifactory/github/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/a.tgz"
            )
        );
        assert_eq!(
            rewritten(&mirrors, "https://api.github.com/repos/o/r?per_page=100").as_deref(),
            Some("https://mirror.example.com/artifactory/github-api/repos/o/r?per_page=100")
        );
        assert_eq!(
            rewritten(&mirrors, "https://GitHub.com:443").as_deref(),
            Some("https://mirror.example.com/artifactory/github/")
        );

        for untouched in [
            "http://github.com/o/r",
            "https://github.com.evil.com/o/r",
            "https://github.com:8443/o/r",
            "https://user@github.com/o/r",
            "https://static.crates.io/crates/a/a-1.0.0.crate",
        ] {
            assert_eq!(rewritten(&mirrors, untouched), None, "{untouched}");
        }
    }

    #[test]
    fn longest_prefix_wins_at_segment_boundaries() {
        let mirrors = mirrors(&[
            "https://github.com/=https://all.example.com/",
            "https://github.com/cargo-bins=https://bins.example.com/",
        ]);

        assert_eq!(
            rewritten(&mirrors, "https://github.com/cargo-bins/cargo-binstall").as_deref(),
            Some("https://bins.example.com/cargo-binstall")
        );
        assert_eq!(
            rewritten(&mirrors, "https://github.com/cargo-binsx/cargo-binstall").as_deref(),
            Some("https://all.example.com/cargo-binsx/cargo-binstall")
        );
    }

    #[test]
    fn replaces_authorization() {
        let mirrors = mirrors(&[
            "https://github.com/=https://mirror.example.com/",
            "https://api.github.com/=https://us%3Aer:p%40ss@api-mirror.example.com/",
        ]);

        let request = |from: &str| {
            let mut request = Request::new(Method::GET, url(from));
            request
                .headers_mut()
                .insert(AUTHORIZATION, HeaderValue::from_static("Bearer gh-token"));
            rewrite(&mirrors, &mut request);
            request
        };

        // The GitHub token is not sent to the mirror.
        let to_mirror = request("https://github.com/o/r");
        assert_eq!(to_mirror.headers().get(AUTHORIZATION), None);

        // The mirror's own credentials are, without staying in the url.
        let to_api_mirror = request("https://api.github.com/graphql");
        assert_eq!(
            to_api_mirror.url().as_str(),
            "https://api-mirror.example.com/graphql"
        );
        assert_eq!(
            to_api_mirror.headers()[AUTHORIZATION],
            // base64("us:er:p@ss")
            "Basic dXM6ZXI6cEBzcw=="
        );
        assert!(!mirrors[1].to_string().contains("p%40ss"));

        // Requests that are not mirrored keep their headers.
        let other = request("https://gitlab.com/o/r");
        assert_eq!(other.headers()[AUTHORIZATION], "Bearer gh-token");
    }

    #[test]
    fn unmirror_maps_back() {
        let mirrors = mirrors(&["https://github.com/=https://mirror.example.com/github/"]);

        assert_eq!(
            unmirror(
                &mirrors,
                &url("https://mirror.example.com/github/Cargo-Bins/x?a#b")
            )
            .unwrap()
            .as_str(),
            "https://github.com/Cargo-Bins/x?a#b"
        );
        assert_eq!(
            unmirror(&mirrors, &url("https://mirror.example.com/gitlab/a")),
            None
        );
    }
}
