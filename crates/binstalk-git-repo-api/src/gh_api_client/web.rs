//! Requests to github.com itself instead of its API: they don't count against
//! the API rate limit, which is 60 requests per hour per IP address without a
//! token.
//!
//! They only work for public repositories, github.com answers 404 for a
//! private one without the user's cookies.

use std::collections::HashSet;

use binstalk_downloader::remote::{self, Method, StatusCode};
use compact_str::CompactString;
use tracing::debug;
use url::Url;

use super::{GhRelease, GhReleaseArtifact, GhRepo};

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

/// Write `line` as a git pkt-line: its length, header included, in four hex
/// digits, then the line itself.
fn write_pkt_line(buf: &mut Vec<u8>, line: &str) {
    buf.extend_from_slice(format!("{:04x}", line.len() + 4).as_bytes());
    buf.extend_from_slice(line.as_bytes());
}

/// The git protocol v2 request listing the refs of `tags`.
fn ls_refs_request<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut request = Vec::new();
    write_pkt_line(&mut request, "command=ls-refs\n");
    // Delimiter packet, between the command and its arguments.
    request.extend_from_slice(b"0001");
    for tag in tags {
        write_pkt_line(&mut request, &format!("ref-prefix refs/tags/{tag}\n"));
    }
    // Flush packet, ending the request.
    request.extend_from_slice(b"0000");
    request
}

/// The tag names in an `ls-refs` response, `None` if it is not one.
///
/// A `ref-prefix` matches every ref starting with it, so this may return more
/// tags than were asked for.
fn parse_ls_refs(mut response: &[u8]) -> Option<HashSet<CompactString>> {
    let mut tags = HashSet::new();
    loop {
        let len = std::str::from_utf8(response.get(..4)?).ok()?;
        let len = usize::from_str_radix(len, 16).ok()?;
        if len == 0 {
            // Flush packet, the end of the list.
            return Some(tags);
        }
        // Fails for the special packets shorter than their header, which
        // don't belong in the list.
        let line = response.get(4..len)?;
        response = &response[len..];

        // `<oid> <ref>`, followed by attributes only if they were asked for.
        let line = std::str::from_utf8(line).ok()?;
        let (oid, line) = line.trim_end_matches('\n').split_once(' ')?;
        if !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            // Most likely an `ERR` packet.
            return None;
        }
        let name = line.split(' ').next()?;
        if let Some(tag) = name.strip_prefix("refs/tags/") {
            tags.insert(tag.into());
        }
    }
}

/// Which of `tags` exist in `repo`, with one request to github.com's git
/// server: git protocol v2 lists only the refs asked for, instead of the
/// hundreds of kilobytes of all the refs of a repository with a long history.
///
/// A tag has a release page whether or not it has a release.
///
/// Returns `None` if the request failed, then check each release page.
pub(super) async fn list_tags<'a>(
    client: &remote::Client,
    GhRepo { owner, repo }: &GhRepo,
    tags: impl IntoIterator<Item = &'a str>,
) -> Option<HashSet<CompactString>> {
    let mut url = Url::parse("https://github.com/").expect("Literal provided must be a valid url");
    url.path_segments_mut()
        .expect("https url can be a base")
        .extend([owner.as_str(), &format!("{repo}.git"), "git-upload-pack"]);

    debug!("Listing tags at: '{url}'");

    let result = async {
        let response = client
            .post(url.clone(), ls_refs_request(tags))
            .header("Git-Protocol", "version=2")
            .header("Content-Type", "application/x-git-upload-pack-request")
            .send(true)
            .await?;
        response.bytes().await
    }
    .await;

    match result {
        Ok(response) => {
            let tags = parse_ls_refs(&response);
            if tags.is_none() {
                debug!("Unexpected response when listing tags at {url}");
            }
            tags
        }
        Err(err) => {
            debug!("Failed to list tags at {url}: {err}");
            None
        }
    }
}

/// The asset names of a release, from [`fetch_release_assets`].
#[derive(Debug)]
pub(super) enum ReleaseAssets {
    /// github.com has no such release.
    NotFound,
    Found(HashSet<CompactString>),
    /// The request failed, or no asset could be found in the page: either
    /// the release has none, or github.com changed the page.
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

/// The names of the assets of `release` linked to from `html`.
fn parse_release_assets(html: &str, release: &GhRelease) -> HashSet<CompactString> {
    let base = Url::parse("https://github.com/").expect("Literal provided must be a valid url");

    html.split("href=\"")
        .skip(1)
        .filter_map(|s| s.split_once('"'))
        .filter_map(|(href, _)| base.join(&unescape_html(href)).ok())
        .filter_map(|url| GhReleaseArtifact::try_extract_from_url(&url))
        // The owner and repository may be spelled differently, or redirected
        // to after a rename.
        .filter(|artifact| artifact.release.tag == release.tag)
        .map(|artifact| artifact.artifact_name)
        .collect()
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
            let assets = parse_release_assets(&String::from_utf8_lossy(&html), release);
            if assets.is_empty() {
                debug!("Found no release assets in {url}");
                ReleaseAssets::Unknown
            } else {
                ReleaseAssets::Found(assets)
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
<a href="https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/absolute.tgz">
"#;

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

    #[test]
    fn test_ls_refs_request() {
        assert_eq!(
            ls_refs_request(["v1.28.0", "1.28.0"]),
            b"0014command=ls-refs\n00010021ref-prefix refs/tags/v1.28.0\n0020ref-prefix refs/tags/1.28.0\n0000"
        );
    }

    #[test]
    fn test_parse_ls_refs() {
        let tags = |response: &[u8]| {
            parse_ls_refs(response).map(|tags| {
                let mut tags: Vec<_> = tags.into_iter().collect();
                tags.sort();
                tags
            })
        };

        // From github.com, plus a tag starting with an asked for one.
        assert_eq!(
            tags(b"003f4b6fa09d0d1ef9740a0c332e31b9ae336f97efa0 refs/tags/v1.28.0\n0042a5c8f6e2d36c0dbf4f6a1e1dc2f1a5b0a6b7c8d9 refs/tags/v1.28.0-rc\n0000"),
            Some(vec!["v1.28.0".into(), "v1.28.0-rc".into()])
        );
        assert_eq!(tags(b"0000"), Some(vec![]));
        assert_eq!(tags(b"000fERR not ok\n0000"), None);
        assert_eq!(tags(b"0001"), None);
        assert_eq!(tags(b"003f4b6fa09d0d1ef9740a0c332e31b9ae336f97efa0"), None);
        assert_eq!(tags(b""), None);
    }
}
