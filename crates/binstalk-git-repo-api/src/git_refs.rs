//! Find out which release tags exist with one request to the git server of a
//! repository, on any git hosting website that speaks git protocol v2: GitHub,
//! GitLab, Codeberg (Forgejo) and so on.
//!
//! Unlike an API, it has no rate limit to spend. Protocol v2 lists only the
//! refs asked for, instead of the hundreds of kilobytes of all the refs of a
//! repository with a long history.

use std::{
    collections::HashSet,
    io::{self, Write},
};

use binstalk_downloader::remote;
use compact_str::CompactString;
use gix_packetline::{
    PacketLineRef,
    blocking_io::encode,
    decode::{self, Stream},
};
use percent_encoding::percent_decode_str;
use tracing::debug;
use url::Url;

use crate::gh_api_client::{GhApiError, check_http_status_and_header};

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

/// The git protocol v2 request listing the refs of `tags`, in pkt-lines.
///
/// Fails only for a tag too long for a pkt-line.
fn ls_refs_request<'a>(tags: impl IntoIterator<Item = &'a str>) -> io::Result<Vec<u8>> {
    let mut request = Vec::new();
    encode::data_to_write(b"command=ls-refs\n", &mut request)?;
    // Between the command and its arguments.
    encode::delim_to_write(&mut request)?;

    let mut line = Vec::new();
    for tag in tags {
        line.clear();
        writeln!(line, "ref-prefix refs/tags/{tag}")?;
        encode::data_to_write(&line, &mut request)?;
    }

    // The end of the request.
    encode::flush_to_write(&mut request)?;
    Ok(request)
}

/// The tag names in an `ls-refs` response, `None` if it is not one.
///
/// A `ref-prefix` matches every ref starting with it, so this may return more
/// tags than were asked for.
fn parse_ls_refs(mut response: &[u8]) -> Option<HashSet<CompactString>> {
    let mut tags = HashSet::new();
    loop {
        let Stream::Complete {
            line,
            bytes_consumed,
        } = decode::streaming(response).ok()?
        else {
            // Cut off before the flush packet that ends the list.
            return None;
        };
        response = &response[bytes_consumed..];

        if let Some(err) = line.check_error() {
            debug!("git server answered: {}", String::from_utf8_lossy(err.0));
            return None;
        }
        let line = match line {
            // The end of the list.
            PacketLineRef::Flush => return Some(tags),
            PacketLineRef::Data(line) => line,
            // Only used between sections, which an `ls-refs` response has
            // none of.
            PacketLineRef::Delimiter | PacketLineRef::ResponseEnd => return None,
        };

        // `<oid> <ref>`, followed by attributes only if they were asked for.
        let line = std::str::from_utf8(line).ok()?;
        let (oid, line) = line.trim_end_matches('\n').split_once(' ')?;
        if !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let name = line.split(' ').next()?;
        if let Some(tag) = name.strip_prefix("refs/tags/") {
            tags.insert(tag.into());
        }
    }
}

/// Which of `tags` exist in the git repository `repo`, with one request,
/// authenticated with `credentials` (username, password) if any.
///
/// Fails if the request failed, e.g. for a private repository without
/// credentials or a server without protocol v2: then check each release some
/// other way.
pub async fn list_tags(
    client: &remote::Client,
    repo: &Url,
    tags: HashSet<CompactString>,
    credentials: Option<(&str, &str)>,
) -> Result<RepoTags, GhApiError> {
    let url = upload_pack_url(repo);

    debug!("Listing tags at: '{url}'");

    let mut request = client.post(url, ls_refs_request(tags.iter().map(|tag| tag.as_str()))?);
    if let Some((username, password)) = credentials {
        request = request.basic_auth(&username, &password);
    }
    let response = request
        .header("Git-Protocol", "version=2")
        .header("Content-Type", "application/x-git-upload-pack-request")
        .send(false)
        .await?;
    let response = check_http_status_and_header(response)?.bytes().await?;

    let existing = parse_ls_refs(&response)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an ls-refs response"))?;

    Ok(RepoTags {
        asked: tags,
        existing,
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
            ls_refs_request(["v1.28.0", "1.28.0"]).unwrap(),
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
        // From gitlab.com, with the attributes git asks for.
        assert_eq!(
            tags(b"006fb1c9d313fc57c7677eae3eb78bddb84bc360f056 refs/tags/v17.0.0 peeled:44feccdf3a5f65891017ef143a78c7a0d22bb63b\n0000"),
            Some(vec!["v17.0.0".into()])
        );
        // Only tags are asked for.
        assert_eq!(
            tags(b"00404b6fa09d0d1ef9740a0c332e31b9ae336f97efa0 refs/heads/v1.28.0\n0000"),
            Some(vec![])
        );
        assert_eq!(tags(b"0000"), Some(vec![]));
        assert_eq!(tags(b"000fERR not ok\n0000"), None);
        // Not an object id.
        assert_eq!(tags(b"0019xyz refs/tags/v1.0.0\n0000"), None);
        assert_eq!(tags(b"0009oops\n0000"), None);
        assert_eq!(tags(b"0001"), None);
        assert_eq!(tags(b"0002"), None);
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
        let list_tags = |repo: &str, tags: &[&str]| {
            let repo = Url::parse(repo).unwrap();
            let tags = tags.iter().copied().map(CompactString::from).collect();
            let client = &client;
            async move { list_tags(client, &repo, tags, None).await }
        };

        let tags = list_tags(
            "https://github.com/cargo-bins/cargo-binstall",
            // We would never release anything older than v0.20.1
            &["v0.20.1", "v0.18.2"],
        )
        .await
        .unwrap();
        assert_eq!(tags.exists("v0.20.1"), Some(true));
        assert_eq!(tags.exists("v0.18.2"), Some(false));
        assert_eq!(tags.exists("v0.20.2"), None);

        // A tag with `+`, and one that is a prefix of other ones
        // (`ripgrep-14.1.10`, ...) which the server lists too.
        let tags = list_tags(
            "https://github.com/cargo-bins/cargo-quickinstall",
            &[
                "cargo-c-0.10.26+cargo-0.100.0",
                "ripgrep-14.1.1",
                "ripgrep-0.0.0",
            ],
        )
        .await
        .unwrap();
        assert_eq!(tags.exists("cargo-c-0.10.26+cargo-0.100.0"), Some(true));
        assert_eq!(tags.exists("ripgrep-14.1.1"), Some(true));
        assert_eq!(tags.exists("ripgrep-0.0.0"), Some(false));

        // Not only github.com.
        let tags = list_tags(
            "https://gitlab.com/gitlab-org/gitlab-runner",
            &["v17.0.0", "17.0.0"],
        )
        .await
        .unwrap();
        assert_eq!(tags.exists("v17.0.0"), Some(true));
        assert_eq!(tags.exists("17.0.0"), Some(false));

        // github.com asks for credentials, as for a private repository.
        let err = list_tags("https://github.com/cargo-bins/ttt", &["v1.0.0"])
            .await
            .unwrap_err();
        assert!(matches!(err, GhApiError::Unauthorized), "{err:?}");
    }
}
