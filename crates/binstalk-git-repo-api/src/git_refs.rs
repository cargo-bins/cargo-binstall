//! Find out which release tags exist with one request to the git server of a
//! repository, on any git hosting website that speaks git protocol v2: GitHub,
//! GitLab, Codeberg (Forgejo) and so on.
//!
//! Unlike an API, it has no rate limit to spend. Protocol v2 lists only the
//! refs asked for, instead of the hundreds of kilobytes of all the refs of a
//! repository with a long history.

use std::collections::HashSet;

use binstalk_downloader::remote;
use compact_str::CompactString;
use percent_encoding::percent_decode_str;
use tracing::debug;
use url::Url;

/// The tag of the release `url` downloads an asset of, if it is one of `repo`.
///
/// Knows the release asset URLs of GitHub and Codeberg
/// (`{repo}/releases/download/{tag}/…`) and GitLab
/// (`{repo}/-/releases/{tag}/downloads/…`).
pub fn release_tag(repo: &str, url: &Url) -> Option<CompactString> {
    let rest = url.as_str().strip_prefix(repo.trim_end_matches('/'))?;

    let tag = if let Some(rest) = rest.strip_prefix("/releases/download/") {
        rest.split_once('/')?.0
    } else {
        let (tag, rest) = rest.strip_prefix("/-/releases/")?.split_once('/')?;
        rest.starts_with("downloads/").then_some(tag)?
    };

    Some(percent_decode_str(tag).decode_utf8().ok()?.into())
}

/// The tags [`list_tags`] was asked about, and which of them exist.
#[derive(Clone, Debug)]
pub struct RepoTags {
    asked: HashSet<CompactString>,
    existing: HashSet<CompactString>,
}

impl RepoTags {
    /// Whether `tag` exists, `None` if it was not asked about.
    pub fn exists(&self, tag: &str) -> Option<bool> {
        self.asked
            .contains(tag)
            .then(|| self.existing.contains(tag))
    }
}

/// The URL git fetches `repo` from.
fn upload_pack_url(repo: &Url) -> Url {
    let mut url = repo.clone();
    url.set_query(None);
    url.set_fragment(None);

    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with(".git") {
        format!("{path}/git-upload-pack")
    } else {
        format!("{path}.git/git-upload-pack")
    };
    url.set_path(&path);
    url
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

/// Which of `tags` exist in the git repository `repo`, with one request.
///
/// Returns `None` if the request failed, e.g. for a private repository or a
/// server without protocol v2, then check each release some other way.
pub async fn list_tags(
    client: &remote::Client,
    repo: &Url,
    tags: HashSet<CompactString>,
) -> Option<RepoTags> {
    let url = upload_pack_url(repo);

    debug!("Listing tags at: '{url}'");

    let result = async {
        let response = client
            .post(
                url.clone(),
                ls_refs_request(tags.iter().map(|tag| tag.as_str())),
            )
            .header("Git-Protocol", "version=2")
            .header("Content-Type", "application/x-git-upload-pack-request")
            .send(true)
            .await?;
        response.bytes().await
    }
    .await;

    let existing = match result {
        Ok(response) => parse_ls_refs(&response),
        Err(err) => {
            debug!("Failed to list tags at {url}: {err}");
            return None;
        }
    };
    if existing.is_none() {
        debug!("Unexpected response when listing tags at {url}");
    }

    Some(RepoTags {
        asked: tags,
        existing: existing?,
    })
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_release_tag() {
        let tag = |repo, url| release_tag(repo, &Url::parse(url).unwrap());

        assert_eq!(
            tag(
                "https://github.com/cargo-bins/cargo-binstall",
                "https://github.com/cargo-bins/cargo-binstall/releases/download/v1.28.0/cargo-binstall-x86_64-unknown-linux-gnu.tgz"
            ),
            Some("v1.28.0".into())
        );
        assert_eq!(
            tag(
                "https://github.com/rustsec/rustsec/",
                "https://github.com/rustsec/rustsec/releases/download/cargo-audit%2Fv0.17.6/cargo-audit.tgz"
            ),
            Some("cargo-audit/v0.17.6".into())
        );
        assert_eq!(
            tag(
                "https://gitlab.com/group/sub/project",
                "https://gitlab.com/group/sub/project/-/releases/v1.0.0/downloads/binaries/project.tgz"
            ),
            Some("v1.0.0".into())
        );
        // Not a release asset.
        assert_eq!(
            tag(
                "https://gitlab.com/group/project",
                "https://gitlab.com/group/project/-/releases/v1.0.0/evidence.json"
            ),
            None
        );
        // Not in the repository.
        assert_eq!(
            tag(
                "https://github.com/cargo-bins/cargo-binstall",
                "https://example.com/releases/download/v1.28.0/cargo-binstall.tgz"
            ),
            None
        );
        // Bitbucket has no tag in its download URLs.
        assert_eq!(
            tag(
                "https://bitbucket.org/owner/repo",
                "https://bitbucket.org/owner/repo/downloads/repo.tgz"
            ),
            None
        );
    }

    #[test]
    fn test_upload_pack_url() {
        let url = |repo| upload_pack_url(&Url::parse(repo).unwrap()).to_string();

        assert_eq!(
            url("https://github.com/cargo-bins/cargo-binstall"),
            "https://github.com/cargo-bins/cargo-binstall.git/git-upload-pack"
        );
        assert_eq!(
            url("https://gitlab.com/group/sub/project.git/"),
            "https://gitlab.com/group/sub/project.git/git-upload-pack"
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

    #[tokio::test]
    async fn test_list_tags() {
        let client = remote::Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
            std::num::NonZeroU16::new(300).unwrap(),
            1.try_into().unwrap(),
            [],
        )
        .unwrap();
        let repo = Url::parse("https://github.com/cargo-bins/cargo-binstall").unwrap();

        let tags = list_tags(
            &client,
            &repo,
            // We would never release anything older than v0.20.1
            ["v0.20.1".into(), "v0.18.2".into()].into(),
        )
        .await
        .unwrap();

        assert_eq!(tags.exists("v0.20.1"), Some(true));
        assert_eq!(tags.exists("v0.18.2"), Some(false));
        assert_eq!(tags.exists("v0.20.2"), None);
    }
}
