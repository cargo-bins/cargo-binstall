//! Requests to github.com itself instead of its API: they don't count against
//! the API rate limit, which is 60 requests per hour per IP address without a
//! token.
//!
//! They only work for public repositories, github.com answers 404 for a
//! private one without the user's cookies.

use std::{collections::HashSet, sync::Arc};

use binstalk_downloader::remote::{self, Method, StatusCode};
use compact_str::CompactString;
use tracing::debug;
use url::Url;

use super::{GhRelease, GhReleaseArtifact, GhRepo, percent_decode_http_url_path};

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

/// Undo the escapes HTML needs in an attribute value.
fn unescape_html(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('&') {
        return s.into();
    }
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .into()
}

/// The URLs linked to from `html`.
fn links(html: &str) -> impl Iterator<Item = Url> + '_ {
    let base = Url::parse("https://github.com/").expect("Literal provided must be a valid url");

    html.split("href=\"")
        .skip(1)
        .filter_map(|s| s.split_once('"'))
        .filter_map(move |(href, _)| base.join(&unescape_html(href)).ok())
}

/// The names of the assets of `release` linked to from `html`.
fn parse_release_assets(html: &str, release: &GhRelease) -> HashSet<CompactString> {
    links(html)
        .filter_map(|url| GhReleaseArtifact::try_extract_from_url(&url))
        // The owner and repository may be spelled differently, or redirected
        // to after a rename.
        .filter(|artifact| artifact.release.tag == release.tag)
        .map(|artifact| artifact.artifact_name)
        .collect()
}

/// Whether `html` links to the source code archive of the tag of `release`,
/// which github.com lists for every release, and also for a tag without one.
fn links_source_archive(html: &str, release: &GhRelease) -> bool {
    let suffix = format!("/archive/refs/tags/{}.tar.gz", release.tag);
    links(html).any(|url| percent_decode_http_url_path(url.path()).ends_with(&*suffix))
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
            let html = String::from_utf8_lossy(&html);
            let assets = parse_release_assets(&html, release);
            // No asset is expected for a tag without a release, or a release
            // without assets: the page then still lists the source code.
            if assets.is_empty() && !links_source_archive(&html, release) {
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
"#;

        assert!(links_source_archive(html, &release));
        assert!(!links_source_archive(
            r#"<a href="/rustsec/rustsec/archive/refs/tags/v0.17.6.tar.gz">"#,
            &release
        ));

        let mut assets: Vec<_> = parse_release_assets(html, &release).into_iter().collect();
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
