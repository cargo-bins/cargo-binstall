//! Requests to github.com itself instead of its API: they don't count against
//! the API rate limit, which is 60 requests per hour per IP address without a
//! token.
//!
//! They only work for public repositories, github.com answers 404 for a
//! private one without the user's cookies.

use std::sync::Arc;

use binstalk_downloader::remote::{self, Method};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use tracing::debug;
use url::Url;

use super::{GhApiError, GhRelease, GhRepo, check_http_status_and_header};

fn release_url(
    GhRelease {
        repo: GhRepo { owner, repo },
        tag,
    }: &GhRelease,
    page: &str,
) -> Url {
    let mut url = Url::parse("https://github.com/").expect("Literal provided must be a valid url");
    url.path_segments_mut()
        .expect("https url can be a base")
        .extend([owner.as_str(), repo, "releases", page, tag]);
    url
}

/// Check whether `release` exists, by HEAD-ing its page.
///
/// Returns `true` unless github.com answers 404: a failed request must not
/// hide a release, it only costs the requests this check was meant to save.
///
/// The page also exists for a tag without a release, then the release's
/// assets are simply not found.
pub(super) async fn has_release_page(client: &remote::Client, release: &GhRelease) -> bool {
    let url = release_url(release, "tag");

    debug!("Checking for release page at: '{url}'");

    let response = client.request(Method::HEAD, url.clone()).send(false).await;
    match response
        .map_err(GhApiError::from)
        .and_then(check_http_status_and_header)
    {
        Ok(_) => true,
        Err(GhApiError::NotFound) => false,
        Err(err) => {
            debug!("Failed to check release page {url}: {err}");
            true
        }
    }
}

/// The asset list of a release, from [`GhApiClient::release_assets_on_web`](super::GhApiClient::release_assets_on_web).
#[derive(Clone, Debug)]
pub enum ReleaseAssets {
    /// github.com has no such release.
    NotFound,
    Found(Arc<ReleaseAssetList>),
    /// The request failed, or the page doesn't look like an asset list:
    /// github.com may have changed it.
    Unknown,
}

/// The asset list github.com shows on the page of a release.
///
/// It comes from a page meant for people, not from an API, so it may not
/// list every asset.
#[derive(Debug)]
pub struct ReleaseAssetList {
    release: GhRelease,
    html: String,
    is_empty: bool,
}

/// What github.com percent-encodes in a path segment of a link, like `/` in a
/// tag.
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

/// Push `segment` as github.com writes it in a link in an HTML attribute.
fn push_path_segment(buf: &mut String, segment: &str) {
    for part in utf8_percent_encode(segment, PATH_SEGMENT) {
        // `"`, `<` and `>` are percent-encoded already.
        for c in part.chars() {
            match c {
                '&' => buf.push_str("&amp;"),
                '\'' => buf.push_str("&#39;"),
                c => buf.push(c),
            }
        }
    }
}

/// Strip `suffix` from `s`, ignoring ASCII case.
fn strip_suffix_ignore_case<'s>(s: &'s str, suffix: &str) -> Option<&'s str> {
    let i = s.len().checked_sub(suffix.len())?;
    let (rest, end) = (s.get(..i)?, &s[i..]);
    end.eq_ignore_ascii_case(suffix).then_some(rest)
}

/// Whether `html` links to `path` in `repo`, i.e. has
/// `href="/{owner}/{repo}{path}`, or with `https://github.com` before it.
///
/// GitHub ignores the case of owner and repository names.
fn links_to(html: &str, GhRepo { owner, repo }: &GhRepo, path: &str) -> bool {
    html.match_indices(path).any(|(i, _)| {
        strip_suffix_ignore_case(&html[..i], repo)
            .and_then(|s| s.strip_suffix('/'))
            .and_then(|s| strip_suffix_ignore_case(s, owner))
            .and_then(|s| s.strip_suffix('/'))
            .map(|s| s.strip_suffix("https://github.com").unwrap_or(s))
            .is_some_and(|s| s.ends_with("href=\""))
    })
}

impl ReleaseAssetList {
    /// Parse the page github.com loads the asset list of `release` from,
    /// `None` if it doesn't look like one.
    fn new(release: &GhRelease, html: String) -> Option<Self> {
        let mut path = String::from("/releases/download/");
        push_path_segment(&mut path, &release.tag);
        path.push('/');

        if links_to(&html, &release.repo, &path) {
            Some(Self {
                release: release.clone(),
                html,
                is_empty: false,
            })
        } else if !html.contains("/releases/download/")
            // github.com lists the source code archives for every release,
            // and also for a tag without one.
            && links_to(&html, &release.repo, "/archive/refs/tags/")
        {
            Some(Self {
                release: release.clone(),
                html,
                is_empty: true,
            })
        } else {
            // It links to assets, but not the way expected for this release.
            None
        }
    }

    /// Whether it lists no asset at all: the release has none, or it is a
    /// tag without a release.
    pub fn is_empty(&self) -> bool {
        self.is_empty
    }

    /// Whether it lists each of `names`, by searching for the link to each.
    pub fn lists<'a>(
        &'a self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> impl Iterator<Item = bool> {
        let mut path = String::from("/releases/download/");
        push_path_segment(&mut path, &self.release.tag);
        path.push('/');
        let prefix_len = path.len();

        names.into_iter().map(move |name| {
            path.truncate(prefix_len);
            push_path_segment(&mut path, name);
            // The end of the attribute, so that `a.tgz` doesn't match
            // `a.tgz.sig`.
            path.push('"');
            !self.is_empty && links_to(&self.html, &self.release.repo, &path)
        })
    }
}

/// Get the asset list of `release` from the page github.com loads it from
/// when showing the release, one request no matter how many assets.
///
/// The page is not documented, so anything unexpected returns
/// [`ReleaseAssets::Unknown`] instead of hiding an asset.
pub(super) async fn fetch_release_assets(
    client: &remote::Client,
    release: &GhRelease,
) -> ReleaseAssets {
    let url = release_url(release, "expanded_assets");

    debug!("Getting release assets from: '{url}'");

    let response = client.get(url.clone()).send(false).await;
    let html = match response
        .map_err(GhApiError::from)
        .and_then(check_http_status_and_header)
    {
        Ok(response) => response.bytes().await,
        Err(GhApiError::NotFound) => return ReleaseAssets::NotFound,
        Err(err) => {
            debug!("Failed to get release assets from {url}: {err}");
            return ReleaseAssets::Unknown;
        }
    };
    let html = match html {
        // Without a copy, the response is not shared.
        Ok(html) => String::from_utf8(html.into()),
        Err(err) => {
            debug!("Failed to get release assets from {url}: {err}");
            return ReleaseAssets::Unknown;
        }
    };

    match html
        .ok()
        .and_then(|html| ReleaseAssetList::new(release, html))
    {
        Some(assets) => ReleaseAssets::Found(Arc::new(assets)),
        None => {
            debug!("{url} does not look like a release asset list");
            ReleaseAssets::Unknown
        }
    }
}

#[cfg(test)]
mod test {
    use compact_str::CompactString;

    use super::*;

    #[test]
    fn test_release_url() {
        let release = GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("rustsec"),
                repo: CompactString::const_new("rustsec"),
            },
            tag: CompactString::const_new("cargo-audit/v0.17.6"),
        };

        assert_eq!(
            release_url(&release, "tag").as_str(),
            "https://github.com/rustsec/rustsec/releases/tag/cargo-audit%2Fv0.17.6"
        );
    }

    fn cargo_audit() -> GhRelease {
        GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("rustsec"),
                repo: CompactString::const_new("rustsec"),
            },
            tag: CompactString::const_new("cargo-audit/v0.17.6"),
        }
    }

    #[test]
    fn test_push_path_segment() {
        let segment = |s| {
            let mut buf = String::new();
            push_path_segment(&mut buf, s);
            buf
        };

        assert_eq!(segment("v0.17.6"), "v0.17.6");
        assert_eq!(segment("cargo-audit/v0.17.6"), "cargo-audit%2Fv0.17.6");
        assert_eq!(
            segment("a b&c'd\"<>%.zip"),
            "a%20b&amp;c&#39;d%22%3C%3E%25.zip"
        );
    }

    #[test]
    fn test_strip_suffix_ignore_case() {
        assert_eq!(strip_suffix_ignore_case("/RustSec", "rustsec"), Some("/"));
        assert_eq!(strip_suffix_ignore_case("rustsec", "rustsec"), Some(""));
        assert_eq!(strip_suffix_ignore_case("sec", "rustsec"), None);
        assert_eq!(strip_suffix_ignore_case("/other", "rustsec"), None);
        // Not a char boundary.
        assert_eq!(strip_suffix_ignore_case("é", "a"), None);
    }

    #[test]
    fn test_release_asset_list() {
        let release = cargo_audit();

        // Shortened from what github.com serves.
        let html = r#"
<a href="/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/cargo-audit-x86_64-unknown-linux-musl-v0.17.6.tgz" rel="nofollow" data-turbo="false" data-view-component="true" class="Truncate">
<a href="/RustSec/RUSTSEC/releases/download/cargo-audit%2Fv0.17.6/a%20b&amp;c.zip" rel="nofollow">
<a href="/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/signed.tgz.sig" rel="nofollow">
<a href="/rustsec/rustsec/releases/download/v0.17.6/other-tag.tgz" rel="nofollow">
<a href="/rustsec/rustsec/archive/refs/tags/cargo-audit/v0.17.6.zip" rel="nofollow">
<a href="/rustsec/rustsec/archive/refs/tags/cargo-audit/v0.17.6.tar.gz" rel="nofollow">
<a href="https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/absolute.tgz">
<a href="/rustsec/other/releases/download/cargo-audit%2Fv0.17.6/other-repo.tgz">
<a href="/other/rustsec/releases/download/cargo-audit%2Fv0.17.6/other-owner.tgz">
<a href="/xrustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/longer-owner.tgz">
<span>/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/not-a-link.tgz"</span>
"#;

        let assets = ReleaseAssetList::new(&release, html.into()).unwrap();
        assert!(!assets.is_empty());

        let names = [
            "cargo-audit-x86_64-unknown-linux-musl-v0.17.6.tgz",
            "a b&c.zip",
            "absolute.tgz",
            "signed.tgz.sig",
            // Only a prefix of the listed name.
            "cargo-audit-x86_64-unknown-linux-musl-v0.17.6",
            "signed.tgz",
            "other-tag.tgz",
            "other-repo.tgz",
            "other-owner.tgz",
            "longer-owner.tgz",
            "not-a-link.tgz",
            "missing.tgz",
        ];
        let listed: Vec<_> = assets.lists(names).collect();
        assert_eq!(
            listed,
            [
                true, true, true, true, false, false, false, false, false, false, false, false
            ]
        );
    }

    #[test]
    fn test_release_asset_list_empty() {
        let release = cargo_audit();

        // A tag without a release, or a release without assets.
        let html = r#"
<a href="/rustsec/rustsec/archive/refs/tags/cargo-audit/v0.17.6.zip" rel="nofollow">
<a href="/rustsec/rustsec/archive/refs/tags/cargo-audit/v0.17.6.tar.gz" rel="nofollow">
"#;
        let assets = ReleaseAssetList::new(&release, html.into()).unwrap();
        assert!(assets.is_empty());
        assert_eq!(assets.lists(["a.tgz"]).collect::<Vec<_>>(), [false]);

        // Not an asset list: github.com may have changed it.
        assert!(ReleaseAssetList::new(&release, String::new()).is_none());
        assert!(
            ReleaseAssetList::new(
                &release,
                r#"<a href="/rustsec/other/archive/refs/tags/cargo-audit/v0.17.6.tar.gz">"#.into()
            )
            .is_none()
        );

        // Asset links not the way expected for this release, e.g. the tag
        // encoded differently: they may be its assets.
        assert!(
            ReleaseAssetList::new(
                &release,
                r#"<a href="/rustsec/rustsec/releases/download/cargo-audit/v0.17.6/a.tgz">
<a href="/rustsec/rustsec/archive/refs/tags/cargo-audit/v0.17.6.tar.gz">"#
                    .into()
            )
            .is_none()
        );
    }
}
