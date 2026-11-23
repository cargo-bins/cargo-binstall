//! Requests to github.com itself instead of its API: they don't count against
//! the API rate limit, which is 60 requests per hour per IP address without a
//! token.
//!
//! They only work for public repositories, github.com answers 404 for a
//! private one without the user's cookies.

use std::fmt;

use binstalk_downloader::remote;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
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

/// What github.com lists of some asset names of a release, from
/// [`ReleaseAssetsPage::lists`].
#[derive(Debug, Eq, PartialEq)]
pub enum ReleaseAssets {
    /// github.com has no such release.
    NotFound,
    /// The release has no assets, or it is a tag without a release.
    NoAssets,
    /// The indices of the names that are listed. The list comes from a page
    /// meant for people, not from an API, so it may not have every asset.
    Listed(Vec<usize>),
    /// The request failed, or the page doesn't look like an asset list:
    /// github.com may have changed it.
    Unknown,
}

/// The page github.com loads the asset list of a release from, without the
/// API, see [`ReleaseAssetsPage::fetch`].
#[derive(Debug)]
pub struct ReleaseAssetsPage(Page);

#[derive(Debug)]
enum Page {
    NotFound,
    Found(ReleaseAssetList),
    Unknown,
}

impl ReleaseAssetsPage {
    /// Get the asset list of `release` from the page github.com loads it
    /// from when showing the release, one request no matter how many assets.
    ///
    /// Only use it for a public repository, github.com answers 404 for a
    /// private one. The page is not documented, so anything unexpected
    /// gives [`ReleaseAssets::Unknown`] instead of hiding an asset.
    pub async fn fetch(client: &remote::Client, release: &GhRelease) -> Self {
        Self(fetch_release_assets(client, release).await)
    }

    /// Which of the asset `names` it lists.
    ///
    /// The list comes from a page meant for people, so an asset missing from
    /// it may still exist, and a listed one should be checked before it is
    /// used.
    pub fn lists<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> ReleaseAssets {
        match &self.0 {
            Page::NotFound => ReleaseAssets::NotFound,
            Page::Found(assets) if assets.is_empty => ReleaseAssets::NoAssets,
            Page::Found(assets) => ReleaseAssets::Listed(assets.lists(names)),
            Page::Unknown => ReleaseAssets::Unknown,
        }
    }
}

/// The asset list github.com shows on the page of a release.
struct ReleaseAssetList {
    release: GhRelease,
    html: String,
    is_empty: bool,
}

/// Without the page, which is large.
impl fmt::Debug for ReleaseAssetList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReleaseAssetList")
            .field("release", &self.release)
            .field("is_empty", &self.is_empty)
            .finish_non_exhaustive()
    }
}

/// What github.com percent-encodes in a tag in a link: everything but
/// letters, digits and `-._~`, e.g. `/` and `+`.
const TAG: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// `/releases/download/{tag}/` as github.com links to it.
///
/// Asset names are written as is, `+` included: GitHub renames an uploaded
/// asset with other special characters in its name. If github.com writes a
/// tag some other way, the page just doesn't look like the asset list of the
/// release.
fn release_download_path(tag: &str) -> String {
    let mut path = String::from("/releases/download/");
    path.extend(utf8_percent_encode(tag, TAG));
    path.push('/');
    path
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
        let is_empty = if links_to(&html, &release.repo, &release_download_path(&release.tag)) {
            false
        } else if !html.contains("/releases/download/")
            // github.com lists the source code archives for every release,
            // and also for a tag without one.
            && links_to(&html, &release.repo, "/archive/refs/tags/")
        {
            true
        } else {
            // It links to assets, but not the way expected for this release.
            return None;
        };

        Some(Self {
            release: release.clone(),
            html,
            is_empty,
        })
    }

    /// The indices of `names` it lists, by searching for the link to each.
    fn lists<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
        let mut path = release_download_path(&self.release.tag);
        let prefix_len = path.len();

        names
            .into_iter()
            .enumerate()
            .filter_map(|(i, name)| {
                path.truncate(prefix_len);
                path.push_str(name);
                // The end of the attribute, so that `a.tgz` doesn't match
                // `a.tgz.sig`.
                path.push('"');
                links_to(&self.html, &self.release.repo, &path).then_some(i)
            })
            .collect()
    }
}

async fn fetch_release_assets(client: &remote::Client, release: &GhRelease) -> Page {
    let url = release_url(release, "expanded_assets");

    debug!("Getting release assets from: '{url}'");

    let response = client.get(url.clone()).send(false).await;
    let html = match response
        .map_err(GhApiError::from)
        .and_then(check_http_status_and_header)
    {
        Ok(response) => response.bytes().await,
        Err(GhApiError::NotFound) => return Page::NotFound,
        Err(err) => {
            debug!("Failed to get release assets from {url}: {err}");
            return Page::Unknown;
        }
    };
    let html = match html {
        // Without a copy, the response is not shared.
        Ok(html) => String::from_utf8(html.into()),
        Err(err) => {
            debug!("Failed to get release assets from {url}: {err}");
            return Page::Unknown;
        }
    };

    match html
        .ok()
        .and_then(|html| ReleaseAssetList::new(release, html))
    {
        Some(assets) => Page::Found(assets),
        None => {
            debug!("{url} does not look like a release asset list");
            Page::Unknown
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
    fn test_release_download_path() {
        assert_eq!(
            release_download_path("v0.17.6"),
            "/releases/download/v0.17.6/"
        );
        assert_eq!(
            release_download_path("cargo-audit/v0.17.6"),
            "/releases/download/cargo-audit%2Fv0.17.6/"
        );
        // As github.com writes the tags of cargo-quickinstall.
        assert_eq!(
            release_download_path("cargo-c-0.10.26+cargo-0.100.0"),
            "/releases/download/cargo-c-0.10.26%2Bcargo-0.100.0/"
        );
        assert_eq!(
            release_download_path("v1.0_rc~1"),
            "/releases/download/v1.0_rc~1/"
        );
        assert_eq!(
            release_download_path("a b\"<>%@é"),
            "/releases/download/a%20b%22%3C%3E%25%40%C3%A9/"
        );
    }

    fn gh_release(owner: &'static str, repo: &'static str, tag: &'static str) -> GhRelease {
        GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new(owner),
                repo: CompactString::const_new(repo),
            },
            tag: CompactString::const_new(tag),
        }
    }

    /// An asset list item as github.com writes it.
    fn asset_link(path: &str) -> String {
        format!(
            r#"<li><div><a href="{path}" rel="nofollow" data-turbo="false" class="wb-break-all"></div></li>"#
        )
    }

    #[test]
    fn test_release_asset_list_cargo_quickinstall() {
        // A tag with `+`: percent-encoded in the tag, not in the name.
        let release = gh_release(
            "cargo-bins",
            "cargo-quickinstall",
            "cargo-c-0.10.26+cargo-0.100.0",
        );
        let html = [
            "/cargo-bins/cargo-quickinstall/releases/download/cargo-c-0.10.26%2Bcargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz",
            "/cargo-bins/cargo-quickinstall/releases/download/cargo-c-0.10.26%2Bcargo-0.100.0/cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz.sig",
            "/cargo-bins/cargo-quickinstall/archive/refs/tags/cargo-c-0.10.26+cargo-0.100.0.zip",
            "/cargo-bins/cargo-quickinstall/archive/refs/tags/cargo-c-0.10.26+cargo-0.100.0.tar.gz",
        ]
        .map(asset_link)
        .concat();
        let assets = ReleaseAssetList::new(&release, html).unwrap();
        assert_eq!(
            assets.lists([
                "cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-gnu.tar.gz",
                "cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz",
                "cargo-c-0.10.26+cargo-0.100.0-x86_64-unknown-linux-musl.tar.gz.sig",
                "cargo-c-0.10.26%2Bcargo-0.100.0-x86_64-unknown-linux-musl.tar.gz",
            ]),
            [1, 2]
        );

        // A tag that is a prefix of another one.
        let release = gh_release("cargo-bins", "cargo-quickinstall", "ripgrep-14.1.1");
        let html = [
            "/cargo-bins/cargo-quickinstall/releases/download/ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz",
            "/cargo-bins/cargo-quickinstall/releases/download/ripgrep-14.1.10/ripgrep-14.1.10-x86_64-unknown-linux-musl.tar.gz",
            "/cargo-bins/cargo-quickinstall/archive/refs/tags/ripgrep-14.1.1.tar.gz",
        ]
        .map(asset_link)
        .concat();
        let assets = ReleaseAssetList::new(&release, html).unwrap();
        assert_eq!(
            assets.lists([
                "ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz",
                "ripgrep-14.1.10-x86_64-unknown-linux-musl.tar.gz",
            ]),
            [0]
        );
    }

    #[test]
    fn test_release_asset_list_cargo_binstall() {
        let release = gh_release("cargo-bins", "cargo-binstall", "v1.28.0");
        let html = [
            "/cargo-bins/cargo-binstall/releases/download/v1.28.0/cargo-binstall-aarch64-apple-darwin.full.zip",
            "/cargo-bins/cargo-binstall/releases/download/v1.28.0/cargo-binstall-aarch64-apple-darwin.full.zip.sig",
            "/cargo-bins/cargo-binstall/releases/download/v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
            "/cargo-bins/cargo-binstall/releases/download/v1.28.0/minisign.pub",
            "/cargo-bins/cargo-binstall/archive/refs/tags/v1.28.0.zip",
            "/cargo-bins/cargo-binstall/archive/refs/tags/v1.28.0.tar.gz",
        ]
        .map(asset_link)
        .concat();
        let assets = ReleaseAssetList::new(&release, html).unwrap();
        assert_eq!(
            assets.lists([
                // Only `.full.zip` is there.
                "cargo-binstall-aarch64-apple-darwin.zip",
                "cargo-binstall-aarch64-apple-darwin.full.zip",
                "cargo-binstall-x86_64-unknown-linux-musl.tgz",
                "cargo-binstall-x86_64-unknown-linux-musl.tar.gz",
                "cargo-binstall-x86_64-unknown-linux-musl",
                "minisign.pub",
            ]),
            [1, 2, 5]
        );

        // The repository in `Cargo.toml` in other case.
        let other_case = GhRelease {
            repo: GhRepo {
                owner: CompactString::const_new("Cargo-Bins"),
                repo: CompactString::const_new("CARGO-BINSTALL"),
            },
            tag: release.tag.clone(),
        };
        let html = asset_link(
            "/cargo-bins/cargo-binstall/releases/download/v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
        );
        let assets = ReleaseAssetList::new(&other_case, html).unwrap();
        assert_eq!(
            assets.lists(["cargo-binstall-x86_64-unknown-linux-musl.tgz"]),
            [0]
        );
    }

    #[test]
    fn test_release_asset_list_unexpected() {
        // A repository that moved: github.com links to the new owner.
        let moved = gh_release("alsuren", "cargo-quickinstall", "ripgrep-14.1.1");
        let html = [
            "/cargo-bins/cargo-quickinstall/releases/download/ripgrep-14.1.1/ripgrep-14.1.1-x86_64-unknown-linux-gnu.tar.gz",
            "/cargo-bins/cargo-quickinstall/archive/refs/tags/ripgrep-14.1.1.tar.gz",
        ]
        .map(asset_link)
        .concat();
        assert!(ReleaseAssetList::new(&moved, html).is_none());

        // Only the assets of another release.
        let release = gh_release("cargo-bins", "cargo-binstall", "v1.28.0");
        let html = [
            "/cargo-bins/cargo-binstall/releases/download/v1.27.0/cargo-binstall-x86_64-unknown-linux-musl.tgz",
            "/cargo-bins/cargo-binstall/archive/refs/tags/v1.28.0.tar.gz",
        ]
        .map(asset_link)
        .concat();
        assert!(ReleaseAssetList::new(&release, html).is_none());

        // An error page, or a login page.
        let html = r#"<html><body><a href="/login">Sign in</a></body></html>"#;
        assert!(ReleaseAssetList::new(&release, html.into()).is_none());
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
<a href="/RustSec/RUSTSEC/releases/download/cargo-audit%2Fv0.17.6/other-case.zip" rel="nofollow">
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
        assert!(!assets.is_empty);

        let names = [
            "cargo-audit-x86_64-unknown-linux-musl-v0.17.6.tgz",
            "other-case.zip",
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
        assert_eq!(assets.lists(names), [0, 1, 2, 3]);
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
        assert!(assets.is_empty);
        assert_eq!(
            ReleaseAssetsPage(Page::Found(assets)).lists(["a.tgz"]),
            ReleaseAssets::NoAssets
        );

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
