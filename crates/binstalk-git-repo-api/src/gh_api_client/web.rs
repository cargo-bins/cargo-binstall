//! Requests to github.com itself instead of its API: they don't count against
//! the API rate limit, which is 60 requests per hour per IP address without a
//! token.
//!
//! They only work for public repositories, github.com answers 404 for a
//! private one without the user's cookies.

use binstalk_downloader::remote::{self, Method, StatusCode};
use tracing::debug;
use url::Url;

use super::{GhRelease, GhRepo};

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

#[cfg(test)]
mod test {
    use super::*;
    use compact_str::CompactString;

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
}
