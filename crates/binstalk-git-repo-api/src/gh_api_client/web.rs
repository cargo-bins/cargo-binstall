//! Requests to github.com itself instead of its API: they don't count against
//! the API rate limit, which is 60 requests per hour per IP address without a
//! token.
//!
//! They only work for public repositories, github.com answers 404 for a
//! private one without the user's cookies.

use std::{borrow::Cow, collections::HashSet, sync::Arc};

use binstalk_downloader::remote::{self, Method, StatusCode};
use compact_str::CompactString;
use percent_encoding::percent_decode_str;
use tracing::debug;
use url::Url;

use super::{GhRelease, GhRepo, percent_decode_http_url_path};

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

    match client.request(Method::HEAD, url.clone()).send(false).await {
        Ok(response) => response.status() != StatusCode::NOT_FOUND,
        Err(err) => {
            debug!("Failed to check release page {url}: {err}");
            true
        }
    }
}

/// The asset names of a release, from [`GhApiClient::release_assets_on_web`](super::GhApiClient::release_assets_on_web).
#[derive(Clone, Debug)]
pub enum ReleaseAssets {
    /// github.com has no such release.
    NotFound,
    /// The names of the assets, which may not be all of them: the list comes
    /// from a page meant for people, not from an API. Empty for a tag
    /// without a release, or a release without assets.
    Found(Arc<HashSet<CompactString>>),
    /// The request failed, or the page doesn't look like an asset list:
    /// github.com may have changed it.
    Unknown,
}

/// Undo the escapes HTML needs in an attribute value, in one pass.
fn unescape_html(s: &str) -> Cow<'_, str> {
    const ENTITIES: [(&str, char); 5] = [
        ("&amp;", '&'),
        ("&quot;", '"'),
        ("&#39;", '\''),
        ("&lt;", '<'),
        ("&gt;", '>'),
    ];

    let Some(first) = s.find('&') else {
        return s.into();
    };

    let mut unescaped = String::with_capacity(s.len());
    unescaped.push_str(&s[..first]);

    let mut rest = &s[first..];
    while let Some(i) = rest.find('&') {
        unescaped.push_str(&rest[..i]);
        rest = &rest[i..];

        // Leave anything else as is.
        let (c, len) = ENTITIES
            .iter()
            .find(|(entity, _)| rest.starts_with(entity))
            .map_or(('&', 1), |(entity, c)| (*c, entity.len()));
        unescaped.push(c);
        rest = &rest[len..];
    }
    unescaped.push_str(rest);

    unescaped.into()
}

/// The values of the `href` attributes in `html`.
fn hrefs(html: &str) -> impl Iterator<Item = Cow<'_, str>> {
    html.match_indices("href=\"")
        .filter_map(|(i, attr)| html[i + attr.len()..].split_once('"'))
        .map(|(href, _)| unescape_html(href))
}

/// The rest of the path of `href` if it links into `repo` on github.com.
///
/// GitHub ignores the case of owner and repository names.
fn path_in_repo<'h>(href: &'h str, repo: &GhRepo) -> Option<&'h str> {
    let path = href.strip_prefix("https://github.com").unwrap_or(href);
    let (owner, path) = path.strip_prefix('/')?.split_once('/')?;
    let (name, path) = path.split_once('/')?;

    (owner.eq_ignore_ascii_case(&repo.owner) && name.eq_ignore_ascii_case(&repo.repo))
        .then_some(path)
}

/// Whether `s` is `expected` once percent-decoded, without allocating.
fn decodes_to(s: &str, expected: &str) -> bool {
    percent_decode_str(s).eq(expected.bytes())
}

/// What the asset list of `release` links to: the names of its assets, and
/// whether the source code archive of its tag is there.
///
/// Each link is matched against the paths of `release`, so links to anything
/// else cost nothing.
fn parse_release_assets(html: &str, release: &GhRelease) -> (HashSet<CompactString>, bool) {
    let mut assets = HashSet::new();
    let mut links_source_archive = false;

    for href in hrefs(html) {
        let Some(path) = path_in_repo(&href, &release.repo) else {
            continue;
        };

        if let Some(path) = path.strip_prefix("releases/download/") {
            // `{tag}/{name}`, with a `/` in the tag percent-encoded.
            if let Some((tag, name)) = path.split_once('/')
                && decodes_to(tag, &release.tag)
                && !name.is_empty()
                && !name.contains('/')
            {
                assets.insert(percent_decode_http_url_path(name));
            }
        } else if let Some(tag) = path
            .strip_prefix("archive/refs/tags/")
            .and_then(|path| path.strip_suffix(".tar.gz"))
        {
            // github.com lists it for every release, and also for a tag
            // without one.
            links_source_archive |= decodes_to(tag, &release.tag);
        }
    }

    (assets, links_source_archive)
}

/// Get the asset names of `release` from the page github.com loads them from
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

    let response = match client.get(url.clone()).send(false).await {
        Ok(response) => response,
        Err(err) => {
            debug!("Failed to get release assets from {url}: {err}");
            return ReleaseAssets::Unknown;
        }
    };

    match response.status() {
        StatusCode::NOT_FOUND => return ReleaseAssets::NotFound,
        status if !status.is_success() => {
            debug!("Failed to get release assets from {url}: {status}");
            return ReleaseAssets::Unknown;
        }
        _ => (),
    }

    match response.bytes().await {
        Ok(html) => {
            let (assets, links_source_archive) =
                parse_release_assets(&String::from_utf8_lossy(&html), release);
            // No asset is expected for a tag without a release, or a release
            // without assets: the page then still lists the source code.
            if assets.is_empty() && !links_source_archive {
                debug!("Found no release assets in {url}");
                ReleaseAssets::Unknown
            } else {
                ReleaseAssets::Found(Arc::new(assets))
            }
        }
        Err(err) => {
            debug!("Failed to get release assets from {url}: {err}");
            ReleaseAssets::Unknown
        }
    }
}

#[cfg(test)]
mod test {
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

    #[test]
    fn test_unescape_html() {
        assert_eq!(unescape_html("a b"), "a b");
        assert_eq!(unescape_html("a&amp;b&quot;&#39;&lt;&gt;"), "a&b\"'<>");
        assert_eq!(unescape_html("&nbsp;&amp"), "&nbsp;&amp");
    }

    #[test]
    fn test_parse_release_assets() {
        let release = GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("rustsec"),
                repo: CompactString::const_new("rustsec"),
            },
            tag: CompactString::const_new("cargo-audit/v0.17.6"),
        };

        // Shortened from what github.com serves.
        let html = r#"
<a href="/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/cargo-audit-x86_64-unknown-linux-musl-v0.17.6.tgz" rel="nofollow" data-turbo="false" data-view-component="true" class="Truncate">
<a href="/RustSec/rustsec/releases/download/cargo-audit%2Fv0.17.6/a%20b&amp;c.zip" rel="nofollow">
<a href="/rustsec/rustsec/releases/download/v0.17.6/other-tag.tgz" rel="nofollow">
<a href="/rustsec/rustsec/archive/refs/tags/cargo-audit/v0.17.6.zip" rel="nofollow">
<a href="/rustsec/rustsec/archive/refs/tags/cargo-audit/v0.17.6.tar.gz" rel="nofollow">
<a href="https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/absolute.tgz">
<a href="/rustsec/other/releases/download/cargo-audit%2Fv0.17.6/other-repo.tgz">
"#;

        let (assets, links_source_archive) = parse_release_assets(html, &release);
        assert!(links_source_archive);
        assert!(
            !parse_release_assets(
                r#"<a href="/rustsec/rustsec/archive/refs/tags/v0.17.6.tar.gz">"#,
                &release
            )
            .1
        );

        let mut assets: Vec<_> = assets.into_iter().collect();
        assets.sort();
        assert_eq!(
            assets,
            [
                "a b&c.zip",
                "absolute.tgz",
                "cargo-audit-x86_64-unknown-linux-musl-v0.17.6.tgz"
            ]
        );
    }
}
