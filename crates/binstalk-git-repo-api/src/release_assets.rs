//! The release assets of a repository on a git hosting website other than
//! GitHub, with one request per release instead of one per asset name the
//! package may have: GitLab, Codeberg (Forgejo), Bitbucket and SourceForge.
//!
//! GitHub has its own way, see [`crate::gh_api_client::ReleaseAssetsPage`].
//!
//! They are asked without a token, so they only work for a public
//! repository, except GitLab with a [`GitLabToken`] for its host. Each answer
//! either lists every asset of the release, or says it may not have, so that
//! an asset it doesn't list is still checked some other way.

use std::{
    collections::{HashMap, HashSet},
    fmt,
};

use binstalk_downloader::remote;
use compact_str::CompactString;
use percent_encoding::percent_decode_str;
use serde::{Deserialize, de::IgnoredAny};
use tracing::debug;
use url::Url;
use zeroize::Zeroizing;

use crate::gh_api_client::{GhApiError, check_http_status_and_header};

/// A git hosting website [`AssetList::fetch`] knows how to ask.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Forge {
    /// gitlab.com, or a GitLab instance of its own.
    GitLab,
    /// codeberg.org, also Gitea.
    Forgejo,
    /// bitbucket.org
    Bitbucket,
    /// sourceforge.net
    SourceForge,
}

/// A token of a GitLab user or CI job, for the API and git of one GitLab
/// instance.
#[derive(Clone)]
pub struct GitLabToken {
    token: Zeroizing<Box<str>>,
    is_job_token: bool,
}

impl fmt::Debug for GitLabToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitLabToken")
            .field("is_job_token", &self.is_job_token)
            .finish_non_exhaustive()
    }
}

impl GitLabToken {
    /// A personal, project, group or OAuth access token.
    pub fn access_token(token: Zeroizing<Box<str>>) -> Self {
        Self {
            token,
            is_job_token: false,
        }
    }

    /// `CI_JOB_TOKEN` of a GitLab CI job.
    pub fn job_token(token: Zeroizing<Box<str>>) -> Self {
        Self {
            token,
            is_job_token: true,
        }
    }

    /// Authenticate a download, which may be redirected to another host.
    ///
    /// Only the `Authorization` header is left out of a redirect to another
    /// host, so a job token goes there as basic auth, not as `JOB-TOKEN`.
    pub fn authenticate_download(&self, request: remote::RequestBuilder) -> remote::RequestBuilder {
        if self.is_job_token {
            let (username, password) = self.git_credentials();
            request.basic_auth(&username, &password)
        } else {
            request.bearer_auth(&&*self.token)
        }
    }

    /// Authenticate an API `request` with it, one that isn't redirected:
    /// see [`GitLabToken::authenticate_download`].
    pub fn authenticate(&self, request: remote::RequestBuilder) -> remote::RequestBuilder {
        if self.is_job_token {
            request.header("JOB-TOKEN", &self.token)
        } else {
            // Takes personal access tokens too.
            request.bearer_auth(&&*self.token)
        }
    }

    /// The username and password git authenticates with.
    pub fn git_credentials(&self) -> (&str, &str) {
        let username = if self.is_job_token {
            "gitlab-ci-token"
        } else {
            // Any username works with an access token.
            "oauth2"
        };
        (username, &self.token)
    }
}

/// The [`GitLabToken`] of each GitLab instance, by host.
///
/// A token is only ever sent to its own host: the repository of a crate can
/// be anywhere.
#[derive(Debug, Default)]
pub struct ForgeTokens {
    gitlab: HashMap<String, GitLabToken>,
}

impl ForgeTokens {
    /// Use `token` for the GitLab instance at `host`, unless there already
    /// is one for it.
    pub fn add_gitlab(&mut self, host: &str, token: GitLabToken) {
        self.gitlab
            .entry(host.to_ascii_lowercase())
            .or_insert(token);
    }

    /// The token of the GitLab instance at the host of `url`, if any.
    pub fn gitlab(&self, url: &Url) -> Option<&GitLabToken> {
        let host = url.host_str()?.to_ascii_lowercase();
        self.gitlab.get(&host)
    }

    pub fn is_empty(&self) -> bool {
        self.gitlab.is_empty()
    }
}

/// Where the assets of a release are listed, see [`locate`].
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct AssetListing {
    kind: Kind,
    /// What is requested.
    url: Url,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum Kind {
    GitLab,
    Forgejo {
        /// Whether a 404 means there is no such release, and not that the
        /// tag isn't understood: Forgejo routes on path segments, so a `/`
        /// in it may not get through as `%2F`.
        trust_not_found: bool,
    },
    Bitbucket,
    SourceForge {
        /// `{repo}/files{folder}/`, percent-decoded: the start of the links
        /// to the files of the folder.
        files: String,
    },
}

/// The release asset a URL downloads, see [`locate`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListedAsset {
    /// Where it is listed, the same for every asset of the release.
    pub listing: AssetListing,
    /// Its name in the listing, percent-decoded.
    pub name: CompactString,
}

fn decode(s: &str) -> Option<CompactString> {
    percent_decode_str(s).decode_utf8().ok().map(Into::into)
}

/// The release asset of `repo` on `forge` that `url` downloads, if it is one
/// and the website lists the assets of its release:
///
/// - GitLab: `{repo}/-/releases/{tag}/downloads/{filepath}`, a release link
///   with a `filepath`, from `/api/v4/projects/{id}/releases/{tag}`.
/// - Forgejo: `{repo}/releases/download/{tag}/{name}`, from
///   `/api/v1/repos/{owner}/{repo}/releases/tags/{tag}`.
/// - Bitbucket: `{repo}/downloads/{name}`, from the repository's downloads
///   on `api.bitbucket.org`.
/// - SourceForge: `{repo}/files/{folder}/{name}/download`, from the RSS feed
///   of the folder.
pub fn locate(forge: Forge, repo: &str, url: &Url) -> Option<ListedAsset> {
    let repo = repo.trim_end_matches('/');
    let rest = url.as_str().strip_prefix(repo)?;
    let repo = Url::parse(repo).ok()?;
    let repo_path = repo.path().trim_matches('/');
    if repo.query().is_some() || repo_path.is_empty() {
        return None;
    }

    let api = |path: &[&str]| {
        let mut api = repo.clone();
        api.set_fragment(None);
        api.path_segments_mut().ok()?.clear().extend(path);
        Some(api)
    };

    let (kind, url, name) = match forge {
        Forge::GitLab => {
            let (tag, rest) = rest.strip_prefix("/-/releases/")?.split_once('/')?;
            let name = rest.strip_prefix("downloads/")?;
            let tag = decode(tag)?;
            // The project is named by its path, subgroups and all.
            let project = decode(repo_path)?;
            let url = api(&["api", "v4", "projects", &project, "releases", &tag])?;
            (Kind::GitLab, url, name)
        }
        Forge::Forgejo => {
            let (owner, name_of_repo) = repo_path.split_once('/')?;
            if name_of_repo.contains('/') {
                return None;
            }
            let (tag, name) = rest.strip_prefix("/releases/download/")?.split_once('/')?;
            let tag = decode(tag)?;
            let url = api(&[
                "api",
                "v1",
                "repos",
                &decode(owner)?,
                &decode(name_of_repo)?,
                "releases",
                "tags",
                &tag,
            ])?;
            let kind = Kind::Forgejo {
                trust_not_found: !tag.contains('/'),
            };
            (kind, url, name)
        }
        Forge::Bitbucket => {
            let (workspace, slug) = repo_path.split_once('/')?;
            if repo.host_str() != Some("bitbucket.org") || slug.contains('/') {
                return None;
            }
            let name = rest.strip_prefix("/downloads/")?;
            let mut url = Url::parse("https://api.bitbucket.org/").ok()?;
            url.path_segments_mut().ok()?.extend([
                "2.0",
                "repositories",
                &decode(workspace)?,
                &decode(slug)?,
                "downloads",
            ]);
            // The most it sends at once.
            url.set_query(Some("pagelen=100"));
            (Kind::Bitbucket, url, name)
        }
        Forge::SourceForge => {
            let (projects, project) = repo_path.split_once('/')?;
            if repo.host_str() != Some("sourceforge.net")
                || projects != "projects"
                || project.contains('/')
            {
                return None;
            }
            let (folder, name) = rest
                .strip_prefix("/files")?
                .strip_suffix("/download")?
                .rsplit_once('/')?;
            let folder = decode(folder)?;
            let files = format!(
                "{}/files{folder}/",
                decode(repo.as_str().trim_end_matches('/'))?
            );

            let mut url = api(&["projects", &decode(project)?, "rss"])?;
            url.query_pairs_mut().append_pair("path", &folder);
            (Kind::SourceForge { files }, url, name)
        }
    };

    let name = decode(name)?;
    if name.is_empty() || (name.contains('/') && !matches!(kind, Kind::GitLab)) {
        return None;
    }
    Some(ListedAsset {
        listing: AssetListing { kind, url },
        name,
    })
}

/// The assets of a release, see [`AssetList::fetch`].
#[derive(Debug)]
pub struct AssetList(List);

#[derive(Debug)]
enum List {
    /// There is no such release.
    NotFound,
    Found {
        names: HashSet<CompactString>,
        /// Whether `names` has every asset: Bitbucket and SourceForge send
        /// the newest ones.
        complete: bool,
        /// The URL each GitLab release link points to, by name.
        links: HashMap<CompactString, Url>,
    },
    /// The request failed, or the answer doesn't look as expected.
    Unknown,
}

impl AssetList {
    /// Get the assets listed at `listing`, with one request.
    ///
    /// `token` is only used for GitLab: get it with [`ForgeTokens::gitlab`]
    /// for [`AssetListing::url`].
    pub async fn fetch(
        client: &remote::Client,
        listing: &AssetListing,
        token: Option<&GitLabToken>,
    ) -> Self {
        let AssetListing { kind, url } = listing;
        debug!("Getting release assets from: '{url}'");

        let get = |token: Option<&GitLabToken>| {
            let mut request = client.get(url.clone());
            if let (Kind::GitLab, Some(token)) = (kind, token) {
                request = token.authenticate(request);
            }
            async move {
                request
                    .send(false)
                    .await
                    .map_err(GhApiError::from)
                    .and_then(check_http_status_and_header)
            }
        };
        let mut token = token.filter(|_| matches!(kind, Kind::GitLab));
        let mut response = get(token).await;
        if token.is_some() && matches!(response, Err(GhApiError::Unauthorized)) {
            // Expired or revoked: a public repository doesn't need it.
            debug!("The token for {url} was refused, trying without it");
            token = None;
            response = get(None).await;
        }
        let body = match response {
            Ok(response) => response.bytes().await,
            Err(GhApiError::NotFound) => {
                return Self(match kind {
                    Kind::Forgejo {
                        trust_not_found: false,
                    } => List::Unknown,
                    _ => List::NotFound,
                });
            }
            Err(err) => {
                debug!("Failed to get release assets from {url}: {err}");
                return Self(List::Unknown);
            }
        };
        let list = match body {
            Ok(body) => parse(kind, &body).map(|mut list| {
                // Downloading from them needs a token that works.
                if let (List::Found { links, .. }, None) = (&mut list, token) {
                    links.clear();
                }
                list
            }),
            Err(err) => {
                debug!("Failed to get release assets from {url}: {err}");
                return Self(List::Unknown);
            }
        };
        Self(list.unwrap_or_else(|| {
            debug!("{url} does not look like a list of release assets");
            List::Unknown
        }))
    }

    /// Whether it lists the asset `name`, `None` if it can't tell.
    pub fn lists(&self, name: &str) -> Option<bool> {
        match &self.0 {
            List::NotFound => Some(false),
            List::Found { names, .. } if names.contains(name) => Some(true),
            List::Found { complete, .. } => complete.then_some(false),
            List::Unknown => None,
        }
    }

    /// Where the GitLab release link `name` points to, if it is listed with
    /// a token that works.
    ///
    /// Unlike `{repo}/-/releases/{tag}/downloads/{filepath}`, which needs the
    /// user to be signed in with a browser for a private repository, a link
    /// to the API of the same instance can be downloaded with a
    /// [`GitLabToken`].
    pub fn link_url(&self, name: &str) -> Option<&Url> {
        match &self.0 {
            List::Found { links, .. } => links.get(name),
            _ => None,
        }
    }

    /// Whether the release exists, `None` if it can't tell.
    #[cfg(test)]
    fn exists(&self) -> Option<bool> {
        match &self.0 {
            List::NotFound => Some(false),
            List::Found { .. } => Some(true),
            List::Unknown => None,
        }
    }
}

impl AssetListing {
    /// What is requested.
    pub fn url(&self) -> &Url {
        &self.url
    }
}

impl fmt::Display for AssetListing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.url.fmt(f)
    }
}

/// What [`parse`] needs of a GitLab release.
#[derive(Deserialize)]
struct GitLabRelease {
    assets: GitLabAssets,
}

#[derive(Deserialize)]
struct GitLabAssets {
    links: Vec<GitLabLink>,
}

#[derive(Deserialize)]
struct GitLabLink {
    url: String,
    direct_asset_url: String,
}

/// What [`parse`] needs of a Forgejo release.
#[derive(Deserialize)]
struct ForgejoRelease {
    assets: Vec<Named>,
}

#[derive(Deserialize)]
struct Named {
    name: CompactString,
}

/// What [`parse`] needs of a page of Bitbucket downloads.
#[derive(Deserialize)]
struct BitbucketDownloads {
    values: Vec<Named>,
    next: Option<IgnoredAny>,
}

/// The most items SourceForge has in a feed.
const SOURCEFORGE_FEED_LEN: usize = 100;

fn parse(kind: &Kind, body: &[u8]) -> Option<List> {
    let mut links = HashMap::new();
    let (names, complete) = match kind {
        Kind::GitLab => {
            let release: GitLabRelease = serde_json::from_slice(body).ok()?;
            let names = release
                .assets
                .links
                .into_iter()
                .filter_map(|link| {
                    // A link without a `filepath` has its own URL instead.
                    let (_, rest) = link.direct_asset_url.split_once("/-/releases/")?;
                    let (_, filepath) = rest.split_once("/downloads/")?;
                    let name = decode(filepath)?;
                    if let Ok(url) = Url::parse(&link.url) {
                        links.insert(name.clone(), url);
                    }
                    Some(name)
                })
                .collect();
            (names, true)
        }
        Kind::Forgejo { .. } => {
            let release: ForgejoRelease = serde_json::from_slice(body).ok()?;
            let names = release.assets.into_iter().map(|asset| asset.name).collect();
            (names, true)
        }
        Kind::Bitbucket => {
            let page: BitbucketDownloads = serde_json::from_slice(body).ok()?;
            let complete = page.next.is_none();
            let names = page.values.into_iter().map(|file| file.name).collect();
            (names, complete)
        }
        Kind::SourceForge { files } => {
            let feed = std::str::from_utf8(body).ok()?;
            if !feed.contains("<rss") {
                return None;
            }
            let items = feed.split("<item>").skip(1);
            let len = items.clone().count();
            let names = items
                .filter_map(|item| {
                    let (_, link) = item.split_once("<link>")?;
                    let (link, _) = link.split_once("</link>")?;
                    let link = decode(&unescape_xml(link))?;
                    let name = link
                        .strip_prefix(files.as_str())?
                        .strip_suffix("/download")?;
                    // In a folder of the folder, the feed has those too.
                    (!name.contains('/')).then(|| name.into())
                })
                .collect();
            (names, len < SOURCEFORGE_FEED_LEN)
        }
    };
    Some(List::Found {
        names,
        complete,
        links,
    })
}

/// Undo the escaping of XML text, which only has the predefined entities in
/// a URL.
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod test {
    use std::num::{NonZeroU16, NonZeroU64};

    use super::*;

    fn located(forge: Forge, repo: &str, url: &str) -> Option<(String, String)> {
        let asset = locate(forge, repo, &Url::parse(url).unwrap())?;
        Some((asset.listing.url.to_string(), asset.name.into()))
    }

    fn pair(url: &str, name: &str) -> Option<(String, String)> {
        Some((url.to_owned(), name.to_owned()))
    }

    #[test]
    fn test_forge_tokens() {
        let mut tokens = ForgeTokens::default();
        assert!(tokens.is_empty());
        let token = |s: &str| Zeroizing::new(Box::<str>::from(s));
        tokens.add_gitlab("GitLab.example.com", GitLabToken::access_token(token("a")));
        // The first one stays.
        tokens.add_gitlab("gitlab.example.com", GitLabToken::job_token(token("b")));

        let url = |s: &str| Url::parse(s).unwrap();
        let found = tokens
            .gitlab(&url("https://gitlab.example.com/a/b/-/releases/v1"))
            .unwrap();
        assert_eq!(found.git_credentials(), ("oauth2", "a"));
        // Not sent anywhere else.
        assert!(tokens.gitlab(&url("https://gitlab.com/a/b")).is_none());
        assert!(
            tokens
                .gitlab(&url("https://gitlab.example.com.evil.com/a/b"))
                .is_none()
        );

        assert_eq!(
            GitLabToken::job_token(token("c")).git_credentials(),
            ("gitlab-ci-token", "c")
        );
        // Not printed.
        let secret = GitLabToken::access_token(token("glpat-secret"));
        assert!(!format!("{secret:?}").contains("glpat-secret"));
    }

    #[test]
    fn test_locate_gitlab() {
        let repo = "https://gitlab.com/gitlab-org/cli";
        assert_eq!(
            located(
                Forge::GitLab,
                repo,
                "https://gitlab.com/gitlab-org/cli/-/releases/v1.131.0/downloads/glab_1.131.0_linux_amd64.tar.gz"
            ),
            pair(
                "https://gitlab.com/api/v4/projects/gitlab-org%2Fcli/releases/v1.131.0",
                "glab_1.131.0_linux_amd64.tar.gz"
            )
        );
        // The default pkg-url of a subcrate, in a subgroup.
        assert_eq!(
            located(
                Forge::GitLab,
                "https://gitlab.example.com/group/sub/project/",
                "https://gitlab.example.com/group/sub/project/-/releases/a-b%2Fv1.0.0/downloads/binaries/a-b-x86_64-unknown-linux-musl.tgz"
            ),
            pair(
                "https://gitlab.example.com/api/v4/projects/group%2Fsub%2Fproject/releases/a-b%2Fv1.0.0",
                "binaries/a-b-x86_64-unknown-linux-musl.tgz"
            )
        );
        // Not a release asset.
        assert_eq!(
            located(
                Forge::GitLab,
                repo,
                "https://gitlab.com/gitlab-org/cli/-/archive/v1.131.0/cli-v1.131.0.tar.gz"
            ),
            None
        );
        // Not of the repository.
        assert_eq!(
            located(
                Forge::GitLab,
                repo,
                "https://gitlab.com/gitlab-org/other/-/releases/v1/downloads/a.tgz"
            ),
            None
        );
    }

    #[test]
    fn test_locate_forgejo() {
        let repo = "https://codeberg.org/forgejo/forgejo";
        assert_eq!(
            located(
                Forge::Forgejo,
                repo,
                "https://codeberg.org/forgejo/forgejo/releases/download/v11.0.0/forgejo-11.0.0-linux-amd64.xz"
            ),
            pair(
                "https://codeberg.org/api/v1/repos/forgejo/forgejo/releases/tags/v11.0.0",
                "forgejo-11.0.0-linux-amd64.xz"
            )
        );
        let asset = locate(
            Forge::Forgejo,
            repo,
            &Url::parse("https://codeberg.org/forgejo/forgejo/releases/download/a%2Fv1/a.tgz")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            asset.listing.url.as_str(),
            "https://codeberg.org/api/v1/repos/forgejo/forgejo/releases/tags/a%2Fv1"
        );
        assert_eq!(
            asset.listing.kind,
            Kind::Forgejo {
                trust_not_found: false
            }
        );
        assert_eq!(
            located(
                Forge::Forgejo,
                repo,
                "https://codeberg.org/forgejo/forgejo/releases/download/v1/a/b.tgz"
            ),
            None
        );
    }

    #[test]
    fn test_locate_bitbucket() {
        assert_eq!(
            located(
                Forge::Bitbucket,
                "https://bitbucket.org/atlassian/bitbucket-upload-file",
                "https://bitbucket.org/atlassian/bitbucket-upload-file/downloads/a-x86_64-unknown-linux-gnu-v1.0.0.tgz"
            ),
            pair(
                "https://api.bitbucket.org/2.0/repositories/atlassian/bitbucket-upload-file/downloads?pagelen=100",
                "a-x86_64-unknown-linux-gnu-v1.0.0.tgz"
            )
        );
    }

    #[test]
    fn test_locate_sourceforge() {
        let asset = locate(
            Forge::SourceForge,
            "https://sourceforge.net/projects/winscp",
            &Url::parse(
                "https://sourceforge.net/projects/winscp/files/WinSCP/6.3.5/WinSCP-6.3.5-Portable.zip/download",
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            asset.listing.url.as_str(),
            "https://sourceforge.net/projects/winscp/rss?path=%2FWinSCP%2F6.3.5"
        );
        assert_eq!(asset.name, "WinSCP-6.3.5-Portable.zip");
        assert_eq!(
            asset.listing.kind,
            Kind::SourceForge {
                files: "https://sourceforge.net/projects/winscp/files/WinSCP/6.3.5/".into()
            }
        );

        assert_eq!(
            located(
                Forge::SourceForge,
                "https://sourceforge.net/projects/winscp",
                "https://sourceforge.net/projects/winscp/files/WinSCP/6.3.5/WinSCP-6.3.5-Portable.zip"
            ),
            None
        );
    }

    fn found(list: &List) -> (Vec<&str>, bool) {
        match list {
            List::Found {
                names, complete, ..
            } => {
                let mut names: Vec<&str> = names.iter().map(|name| name.as_str()).collect();
                names.sort_unstable();
                (names, *complete)
            }
            list => panic!("{list:?}"),
        }
    }

    #[test]
    fn test_parse_gitlab() {
        let body = br#"{"tag_name":"v1.131.0","assets":{"count":3,"sources":[{"format":"zip","url":"https://gitlab.com/gitlab-org/cli/-/archive/v1.131.0/cli-v1.131.0.zip"}],"links":[
            {"id":1,"name":"glab_1.131.0_linux_amd64.tar.gz","url":"https://gitlab.com/api/v4/projects/gitlab-org%2Fcli/packages/generic/glab/1%2E131%2E0/glab_1%2E131%2E0_linux_amd64%2Etar%2Egz","direct_asset_url":"https://gitlab.com/gitlab-org/cli/-/releases/v1.131.0/downloads/glab_1.131.0_linux_amd64.tar.gz","link_type":"other"},
            {"id":2,"name":"in a folder","url":"https://example.com/a","direct_asset_url":"https://gitlab.com/gitlab-org/cli/-/releases/v1.131.0/downloads/binaries/a%20b.tgz","link_type":"package"},
            {"id":3,"name":"no filepath","url":"https://example.com/b.tgz","direct_asset_url":"https://example.com/b.tgz","link_type":"other"}
        ]}}"#;
        let list = parse(&Kind::GitLab, body).unwrap();
        assert_eq!(
            found(&list),
            (
                vec!["binaries/a b.tgz", "glab_1.131.0_linux_amd64.tar.gz"],
                true
            )
        );

        let list = AssetList(list);
        assert_eq!(
            list.link_url("glab_1.131.0_linux_amd64.tar.gz")
                .unwrap()
                .as_str(),
            "https://gitlab.com/api/v4/projects/gitlab-org%2Fcli/packages/generic/glab/1%2E131%2E0/glab_1%2E131%2E0_linux_amd64%2Etar%2Egz"
        );
        assert_eq!(list.link_url("glab.zip"), None);

        assert!(parse(&Kind::GitLab, br#"{"message":"403 Forbidden"}"#).is_none());
    }

    #[test]
    fn test_parse_forgejo() {
        let body = br#"{"id":1,"tag_name":"v11.0.0","draft":false,"assets":[
            {"id":1,"name":"forgejo-11.0.0-linux-amd64.xz","size":1,"download_count":0,"created_at":"2025-10-16T00:00:00Z","uuid":"x","browser_download_url":"https://codeberg.org/forgejo/forgejo/releases/download/v11.0.0/forgejo-11.0.0-linux-amd64.xz","type":"attachment"},
            {"id":2,"name":"a+b c.tgz","browser_download_url":"https://codeberg.org/forgejo/forgejo/releases/download/v11.0.0/a+b%20c.tgz"}
        ]}"#;
        let list = parse(
            &Kind::Forgejo {
                trust_not_found: true,
            },
            body,
        )
        .unwrap();
        assert_eq!(
            found(&list),
            (vec!["a+b c.tgz", "forgejo-11.0.0-linux-amd64.xz"], true)
        );
    }

    #[test]
    fn test_parse_bitbucket() {
        let body = br#"{"values":[{"name":"a.tgz","size":1,"links":{"self":{"href":"https://api.bitbucket.org/2.0/repositories/w/r/downloads/a.tgz"}}}],"pagelen":1,"size":2,"page":1,"next":"https://api.bitbucket.org/2.0/repositories/w/r/downloads?pagelen=1&page=2"}"#;
        let list = parse(&Kind::Bitbucket, body).unwrap();
        assert_eq!(found(&list), (vec!["a.tgz"], false));
        let list = AssetList(list);
        assert_eq!(list.lists("a.tgz"), Some(true));
        // Maybe on the next page.
        assert_eq!(list.lists("b.tgz"), None);

        let list = parse(
            &Kind::Bitbucket,
            br#"{"values":[],"pagelen":100,"size":0,"page":1}"#,
        )
        .unwrap();
        assert_eq!(found(&list), (vec![], true));
        assert_eq!(AssetList(list).lists("a.tgz"), Some(false));
    }

    #[test]
    fn test_parse_sourceforge() {
        let kind = Kind::SourceForge {
            files: "https://sourceforge.net/projects/p/files/binaries/1.0.0/".into(),
        };
        let item = |path: &str| {
            format!(
                "<item><title><![CDATA[{path}]]></title><link>https://sourceforge.net/projects/p/files{path}/download</link></item>"
            )
        };
        let feed = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><rss version="2.0"><channel><title>p</title><link>https://sourceforge.net</link>{}{}{}</channel></rss>"#,
            item("/binaries/1.0.0/p-x86_64-unknown-linux-gnu.tgz"),
            item("/binaries/1.0.0/a%20b&amp;c.zip"),
            item("/binaries/1.0.0/sub/p.tgz"),
        );
        let list = parse(&kind, feed.as_bytes()).unwrap();
        assert_eq!(
            found(&list),
            (vec!["a b&c.zip", "p-x86_64-unknown-linux-gnu.tgz"], true)
        );

        let full = format!(
            "<rss>{}</rss>",
            item("/binaries/1.0.0/x.tgz").repeat(SOURCEFORGE_FEED_LEN)
        );
        assert!(!found(&parse(&kind, full.as_bytes()).unwrap()).1);

        // A page of the website, not a feed.
        assert!(parse(&kind, b"<!doctype html><html></html>").is_none());
    }

    fn client() -> remote::Client {
        remote::Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
            NonZeroU16::new(10).unwrap(),
            NonZeroU64::new(1).unwrap(),
            [],
        )
        .unwrap()
    }

    async fn fetch(forge: Forge, repo: &str, url: &str) -> (ListedAsset, AssetList) {
        let asset = locate(forge, repo, &Url::parse(url).unwrap()).unwrap();
        let list = AssetList::fetch(&client(), &asset.listing, None).await;
        (asset, list)
    }

    #[tokio::test]
    async fn test_fetch_gitlab() {
        let repo = "https://gitlab.com/gitlab-org/cli";
        let (asset, list) = fetch(
            Forge::GitLab,
            repo,
            "https://gitlab.com/gitlab-org/cli/-/releases/v1.131.0/downloads/glab_1.131.0_linux_amd64.tar.gz",
        )
        .await;
        assert_eq!(list.lists(&asset.name), Some(true));
        assert_eq!(list.lists("glab_1.131.0_linux_amd64.tar.xz"), Some(false));

        // A tag that isn't a release.
        let (asset, list) = fetch(
            Forge::GitLab,
            repo,
            "https://gitlab.com/gitlab-org/cli/-/releases/v0.0.0-no-such-release/downloads/glab.tgz",
        )
        .await;
        assert_eq!(list.exists(), Some(false));
        assert_eq!(list.lists(&asset.name), Some(false));
    }

    #[tokio::test]
    async fn test_fetch_forgejo() {
        let repo = "https://codeberg.org/forgejo/forgejo";
        let (asset, list) = fetch(
            Forge::Forgejo,
            repo,
            "https://codeberg.org/forgejo/forgejo/releases/download/v11.0.0/forgejo-11.0.0-linux-amd64.xz",
        )
        .await;
        assert_eq!(list.lists(&asset.name), Some(true));
        assert_eq!(list.lists("forgejo-11.0.0-linux-amd64.tgz"), Some(false));

        let (_, list) = fetch(
            Forge::Forgejo,
            repo,
            "https://codeberg.org/forgejo/forgejo/releases/download/v0.0.0-no-such-release/a.tgz",
        )
        .await;
        assert_eq!(list.exists(), Some(false));
    }

    #[tokio::test]
    async fn test_fetch_bitbucket() {
        let (asset, list) = fetch(
            Forge::Bitbucket,
            "https://bitbucket.org/atlassian/bitbucket-upload-file",
            "https://bitbucket.org/atlassian/bitbucket-upload-file/downloads/CONTRIBUTING.md",
        )
        .await;
        assert_eq!(list.lists(&asset.name), Some(true));
        assert_eq!(list.lists("a-x86_64-unknown-linux-gnu.tgz"), Some(false));
    }

    #[tokio::test]
    async fn test_fetch_sourceforge() {
        let repo = "https://sourceforge.net/projects/winscp";
        let (asset, list) = fetch(
            Forge::SourceForge,
            repo,
            "https://sourceforge.net/projects/winscp/files/WinSCP/6.3.5/WinSCP-6.3.5-Portable.zip/download",
        )
        .await;
        assert_eq!(list.lists(&asset.name), Some(true));
        assert_eq!(list.lists("WinSCP-6.3.5-Portable.tgz"), Some(false));

        let (_, list) = fetch(
            Forge::SourceForge,
            repo,
            "https://sourceforge.net/projects/winscp/files/WinSCP/0.0.0/a.tgz/download",
        )
        .await;
        assert_eq!(list.exists(), Some(false));
    }
}
