//! Checksum files published next to a package, e.g. `{ pkg }.sha256`, which
//! many releases have: if there is one, the package must match it.
//!
//! Such a file comes from the same place as the package, so it catches a
//! broken or truncated download, or an asset replaced without its checksum,
//! not a release made by someone else: that is what signatures are for, see
//! `SIGNING.md`.
//!
//! For a GitHub release, the asset list says which checksum files there
//! are, normally already fetched to find the package, so a release without
//! any costs no request. Anywhere else, the most common two are tried with
//! GET.

use binstalk_downloader::remote::StatusCode;
use binstalk_git_repo_api::gh_api_client::{
    GhApiError, GhReleaseArtifact, GhReleaseArtifactUrl, ReleaseAssets, ReleaseAssetsPage,
};
use bytes::{Bytes, BytesMut};
use percent_encoding::percent_decode_str;
use sha2::{Digest, Sha256, Sha512};
use tokio::time::sleep;

use crate::{Data, FetchError, common::*, gh_api_rate_limit_wait};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Algorithm {
    Sha256,
    Sha512,
}

impl Algorithm {
    /// Length of the digest in bytes.
    fn len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha512 => 64,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }
}

struct Sidecar {
    suffix: &'static str,
    algorithm: Algorithm,
    /// Whether it is tried with GET when the release's asset list is not
    /// known: each one tried costs a request for every package without
    /// one.
    probe: bool,
}

/// The checksum files looked for, appended to the package's file name, most
/// preferred first.
static SIDECARS: [Sidecar; 4] = [
    Sidecar {
        suffix: ".sha512",
        algorithm: Algorithm::Sha512,
        probe: true,
    },
    Sidecar {
        suffix: ".sha512sum",
        algorithm: Algorithm::Sha512,
        probe: false,
    },
    Sidecar {
        suffix: ".sha256",
        algorithm: Algorithm::Sha256,
        probe: true,
    },
    Sidecar {
        suffix: ".sha256sum",
        algorithm: Algorithm::Sha256,
        probe: false,
    },
];

/// Upper bound on the size of a checksum file worth reading: it normally
/// has one line.
const MAX_CHECKSUM_FILE_SIZE: usize = 64 * 1024;

/// The checksum a package must have.
#[derive(Debug)]
pub(super) struct Checksum {
    pub(super) algorithm: Algorithm,
    expected: Box<[u8]>,
    /// Where it was read from.
    pub(super) url: Url,
}

impl Checksum {
    pub(super) fn verifier(&self) -> ChecksumVerifier<'_> {
        ChecksumVerifier {
            checksum: self,
            state: Some(match self.algorithm {
                Algorithm::Sha256 => Hasher::Sha256(Sha256::new()),
                Algorithm::Sha512 => Hasher::Sha512(Sha512::new()),
            }),
            actual: None,
        }
    }

    pub(super) fn expected_hex(&self) -> String {
        base16::encode_lower(&self.expected)
    }
}

enum Hasher {
    Sha256(Sha256),
    Sha512(Sha512),
}

/// Checks that the data downloaded has the expected checksum.
pub(super) struct ChecksumVerifier<'a> {
    checksum: &'a Checksum,
    state: Option<Hasher>,
    actual: Option<Box<[u8]>>,
}

impl ChecksumVerifier<'_> {
    pub(super) fn update(&mut self, data: &[u8]) {
        match &mut self.state {
            Some(Hasher::Sha256(state)) => state.update(data),
            Some(Hasher::Sha512(state)) => state.update(data),
            None => debug_assert!(false, "update after validate"),
        }
    }

    /// Return true if the data has the expected checksum.
    pub(super) fn validate(&mut self) -> bool {
        if let Some(state) = self.state.take() {
            self.actual = Some(match state {
                Hasher::Sha256(state) => state.finalize().to_vec().into(),
                Hasher::Sha512(state) => state.finalize().to_vec().into(),
            });
        }
        self.actual.as_deref() == Some(&*self.checksum.expected)
    }

    pub(super) fn actual_hex(&self) -> String {
        self.actual
            .as_deref()
            .map(base16::encode_lower)
            .unwrap_or_default()
    }
}

/// The name of the file `url` downloads, which its checksum file is named
/// after, if it is the last segment of its path.
fn file_name(url: &Url) -> Option<String> {
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    // Its URLs end with `/download`, and it answers anything with a page.
    if url
        .domain()
        .is_some_and(|domain| domain == "sourceforge.net" || domain.ends_with(".sourceforge.net"))
    {
        return None;
    }
    let name = url.path_segments()?.next_back()?;
    let name = percent_decode_str(name).decode_utf8().ok()?;
    (!name.is_empty()).then(|| name.into_owned())
}

fn with_suffix(url: &Url, suffix: &str) -> Url {
    let mut url = url.clone();
    url.set_path(&format!("{}{suffix}", url.path()));
    url
}

/// Find the checksum file published next to the package at `url`, if any,
/// and read the package's checksum from it.
///
/// A checksum file that can't be found or read is warned about and
/// skipped: the package is then installed as if there was none.
pub(super) async fn find(
    client: &Client,
    gh_api_client: &GhApiClient,
    data: &Data,
    url: &Url,
    is_repo_private: bool,
) -> Option<Checksum> {
    find_with(&SIDECARS, client, gh_api_client, data, url, is_repo_private).await
}

async fn find_with(
    sidecars: &'static [Sidecar],
    client: &Client,
    gh_api_client: &GhApiClient,
    data: &Data,
    url: &Url,
    is_repo_private: bool,
) -> Option<Checksum> {
    let name = file_name(url)?;

    if let Some(artifact) = GhReleaseArtifact::try_extract_from_url(url) {
        if let Some(listed) = listed_on_github(
            sidecars,
            client,
            gh_api_client,
            data,
            &artifact,
            is_repo_private,
        )
        .await
        {
            for (sidecar, artifact_url) in listed {
                let sidecar_url = with_suffix(url, sidecar.suffix);
                let artifact_url = artifact_url.filter(|_| is_repo_private);
                let Some(contents) =
                    read_small_file(client, gh_api_client, &sidecar_url, artifact_url).await
                else {
                    continue;
                };
                if let Some(checksum) = parse(sidecar, &sidecar_url, &contents, &name) {
                    return Some(checksum);
                }
            }
            return None;
        }
        // A private repository's assets can only be downloaded with the API.
        if is_repo_private {
            return None;
        }
    }

    // Tried at once, as normally none of them exists.
    let probes = sidecars
        .iter()
        .filter(|sidecar| sidecar.probe)
        .map(|sidecar| {
            let sidecar_url = with_suffix(url, sidecar.suffix);
            async move {
                debug!("Checking for checksum file at: '{sidecar_url}'");
                let contents = read_small_file(client, gh_api_client, &sidecar_url, None).await;
                (sidecar, sidecar_url, contents)
            }
        });
    for (sidecar, sidecar_url, contents) in futures_util::future::join_all(probes).await {
        if let Some(checksum) = contents
            .as_deref()
            .and_then(|contents| parse(sidecar, &sidecar_url, contents, &name))
        {
            return Some(checksum);
        }
    }
    None
}

/// The checksum files the GitHub release of `artifact` has, most preferred
/// first, with the URLs to download them with the API if known.
///
/// `None` if its asset list can't tell, or can't be had.
async fn listed_on_github(
    sidecars: &'static [Sidecar],
    client: &Client,
    gh_api_client: &GhApiClient,
    data: &Data,
    artifact: &GhReleaseArtifact,
    is_repo_private: bool,
) -> Option<Vec<(&'static Sidecar, Option<GhReleaseArtifactUrl>)>> {
    let names: Vec<String> = sidecars
        .iter()
        .map(|sidecar| format!("{}{}", artifact.artifact_name, sidecar.suffix))
        .collect();

    // The API's answer, if finding the package asked it, or if it must be
    // asked: a private repository has no other.
    let cell = data.repo_cache.release_artifacts(&artifact.release);
    if is_repo_private && !cell.initialized() {
        loop {
            match cell
                .get_or_try_init(|| get_gh_release_artifacts(gh_api_client, &artifact.release))
                .await
            {
                Ok(_) => break,
                Err(GhApiError::RateLimit { retry_after }) => {
                    match gh_api_rate_limit_wait(retry_after) {
                        Some(wait) => sleep(wait).await,
                        None => {
                            warn!(
                                "Cannot look for checksum files of {artifact:?}: GitHub API rate limited"
                            );
                            return None;
                        }
                    }
                }
                Err(err) => {
                    warn!("Cannot look for checksum files of {artifact:?}: {err}");
                    return None;
                }
            }
        }
    }
    if let Some(artifacts) = cell.get() {
        let Some(artifacts) = artifacts else {
            return Some(Vec::new());
        };
        return Some(
            sidecars
                .iter()
                .zip(&names)
                .filter_map(|(sidecar, name)| Some((sidecar, Some(artifacts.artifact_url(name)?))))
                .collect(),
        );
    }

    // The asset list on github.com, normally already fetched to find the
    // package.
    let page = data.repo_cache.release_assets_page(&artifact.release);
    let page = page
        .get_or_init(|| ReleaseAssetsPage::fetch(client, &artifact.release))
        .await;
    match page.lists(
        std::iter::once(artifact.artifact_name.as_str()).chain(names.iter().map(String::as_str)),
    ) {
        // It lists the package, so it is not missing assets that the
        // checksum files would be.
        ReleaseAssets::Listed(listed) if listed.contains(&0) => Some(
            listed
                .into_iter()
                .filter_map(|i| Some((sidecars.get(i.checked_sub(1)?)?, None)))
                .collect(),
        ),
        _ => None,
    }
}

/// Read `url` if it exists, using the API with `artifact_url` if given.
///
/// Any failure is logged and gives `None`: the checksum file is then
/// skipped.
async fn read_small_file(
    client: &Client,
    gh_api_client: &GhApiClient,
    url: &Url,
    artifact_url: Option<GhReleaseArtifactUrl>,
) -> Option<Bytes> {
    let result: Result<Option<Bytes>, FetchError> = async {
        if let Some(artifact_url) = artifact_url {
            let bytes = gh_api_client
                .download_artifact(artifact_url)
                .await?
                .into_bytes()
                .await?;
            return Ok((bytes.len() <= MAX_CHECKSUM_FILE_SIZE).then_some(bytes));
        }

        let mut response = client.get(url.clone()).send(false).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        response.error_for_status_ref()?;

        let mut body = BytesMut::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > MAX_CHECKSUM_FILE_SIZE {
                return Ok(None);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body.freeze()))
    }
    .await;

    match result {
        Ok(Some(bytes)) => Some(bytes),
        Ok(None) => {
            debug!("No checksum file at '{url}', or it is too large");
            None
        }
        Err(err) => {
            warn!("Failed to get checksum file {url}, ignoring it: {err}");
            None
        }
    }
}

fn parse(sidecar: &Sidecar, url: &Url, contents: &[u8], name: &str) -> Option<Checksum> {
    match parse_checksum_file(contents, sidecar.algorithm, name) {
        Ok(expected) => Some(Checksum {
            algorithm: sidecar.algorithm,
            expected,
            url: url.clone(),
        }),
        Err(err) => {
            warn!("Ignoring checksum file {url}: {err}");
            None
        }
    }
}

/// Read the checksum of the file `name` from a checksum file, in the format
/// `sha256sum` writes, `<hex>  <name>`, or `sha256sum --tag` writes,
/// `SHA256 (<name>) = <hex>`, or just `<hex>`. A file name that is a path
/// matches by its last component.
fn parse_checksum_file(
    contents: &[u8],
    algorithm: Algorithm,
    name: &str,
) -> Result<Box<[u8]>, &'static str> {
    let contents = std::str::from_utf8(contents).map_err(|_| "it is not UTF-8")?;

    let tag = match algorithm {
        Algorithm::Sha256 => "SHA256",
        Algorithm::Sha512 => "SHA512",
    };

    let mut found = None;
    let mut lines = 0;
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        lines += 1;

        let (hex, file) = if let Some(rest) = line
            .strip_prefix(tag)
            .and_then(|rest| rest.trim_start().strip_prefix('('))
        {
            // `SHA256 (<name>) = <hex>`
            let (file, hex) = rest.rsplit_once('=').ok_or("it is not in a known format")?;
            let file = file
                .trim_end()
                .strip_suffix(')')
                .ok_or("it is not in a known format")?;
            (hex.trim(), Some(file))
        } else {
            match line.split_once(char::is_whitespace) {
                // `<hex>  <name>`, or `<hex> *<name>` for binary mode
                Some((hex, file)) => {
                    let file = file.trim_start();
                    (hex, Some(file.strip_prefix('*').unwrap_or(file)))
                }
                None => (line, None),
            }
        };

        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("it is not in a known format");
        }

        let matches = file.is_none_or(|file| {
            file.rsplit(['/', '\\'])
                .next()
                .is_some_and(|file| file == name)
        });
        if !matches {
            continue;
        }

        let mut digest = vec![0; algorithm.len()].into_boxed_slice();
        if hex.len() != algorithm.len() * 2
            || base16::decode_slice(hex, &mut digest) != Ok(algorithm.len())
        {
            return Err(match algorithm {
                Algorithm::Sha256 => "it does not have a valid sha256 checksum",
                Algorithm::Sha512 => "it does not have a valid sha512 checksum",
            });
        }
        if found.as_ref().is_some_and(|found| *found != digest) {
            return Err("it has different checksums for the package");
        }
        found = Some(digest);
    }

    match (found, lines) {
        (Some(digest), _) => Ok(digest),
        (None, 0) => Err("it is empty"),
        (None, _) => Err("it has no checksum for the package"),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const SHA256: &str = "7fee44ff3ff71ed0064b32d4ec62fee6b58bd37c5887b12a86aedddc5af0c7b5";
    const NAME: &str = "app-x86_64-unknown-linux-musl.tar.gz";

    fn parse_sha256(contents: &str) -> Result<String, &'static str> {
        parse_checksum_file(contents.as_bytes(), Algorithm::Sha256, NAME)
            .map(|digest| base16::encode_lower(&digest))
    }

    #[test]
    fn formats() {
        let expected = Ok(SHA256.to_string());

        // Just the hex, as sccache publishes.
        assert_eq!(parse_sha256(SHA256), expected);
        assert_eq!(parse_sha256(&format!("{SHA256}\n")), expected);
        // Upper case hex.
        assert_eq!(parse_sha256(&SHA256.to_uppercase()), expected);
        // `sha256sum` output, as git-cliff publishes.
        assert_eq!(parse_sha256(&format!("{SHA256}  {NAME}\n")), expected);
        // Binary mode.
        assert_eq!(parse_sha256(&format!("{SHA256} *{NAME}\n")), expected);
        // A path, from running `sha256sum` outside of the directory.
        assert_eq!(
            parse_sha256(&format!("{SHA256}  ./target/dist/{NAME}\n")),
            expected
        );
        assert_eq!(
            parse_sha256(&format!("{SHA256}  dist\\{NAME}\r\n")),
            expected
        );
        // BSD style, `sha256sum --tag` or `shasum -a 256 --tag`.
        assert_eq!(
            parse_sha256(&format!("SHA256 ({NAME}) = {SHA256}\n")),
            expected
        );
        // All the packages of the release.
        assert_eq!(
            parse_sha256(&format!(
                "# checksums\n{}  other.tar.gz\n{SHA256}  {NAME}\n",
                "0".repeat(64)
            )),
            expected
        );
    }

    #[test]
    fn rejected() {
        assert_eq!(parse_sha256(""), Err("it is empty"));
        assert_eq!(parse_sha256("\n# nothing\n"), Err("it is empty"));
        assert_eq!(
            parse_sha256(&format!("{SHA256}  other.tar.gz\n")),
            Err("it has no checksum for the package")
        );
        // A prefix of the name is another file.
        assert_eq!(
            parse_sha256(&format!("{SHA256}  x{NAME}\n")),
            Err("it has no checksum for the package")
        );
        assert_eq!(
            parse_sha256(&SHA256[1..]),
            Err("it does not have a valid sha256 checksum")
        );
        assert_eq!(
            parse_sha256(&format!("{}g", &SHA256[1..])),
            Err("it is not in a known format")
        );
        // A sha512 checksum in a `.sha256` file.
        assert_eq!(
            parse_sha256(&SHA256.repeat(2)),
            Err("it does not have a valid sha256 checksum")
        );
        assert_eq!(
            parse_sha256(&format!("{SHA256}\n{}\n", "0".repeat(64))),
            Err("it has different checksums for the package")
        );
        assert_eq!(
            parse_sha256("<!DOCTYPE html>"),
            Err("it is not in a known format")
        );
        assert_eq!(
            parse_checksum_file(&[0xff], Algorithm::Sha256, NAME),
            Err("it is not UTF-8")
        );
    }

    #[test]
    fn sha512() {
        let data = b"some package";
        let hex = base16::encode_lower(&Sha512::digest(data));
        let expected = parse_checksum_file(
            format!("SHA512 ({NAME}) = {hex}").as_bytes(),
            Algorithm::Sha512,
            NAME,
        )
        .unwrap();
        assert_eq!(base16::encode_lower(&expected), hex);
    }

    fn checksum(algorithm: Algorithm, expected: &[u8]) -> Checksum {
        Checksum {
            algorithm,
            expected: expected.into(),
            url: Url::parse("https://example.com/a.sha256").unwrap(),
        }
    }

    #[test]
    fn verifier() {
        let data = b"some package";
        for (algorithm, expected) in [
            (Algorithm::Sha256, Sha256::digest(data).to_vec()),
            (Algorithm::Sha512, Sha512::digest(data).to_vec()),
        ] {
            let checksum = checksum(algorithm, &expected);

            let mut verifier = checksum.verifier();
            verifier.update(&data[..4]);
            verifier.update(&data[4..]);
            assert!(verifier.validate());
            assert_eq!(verifier.actual_hex(), checksum.expected_hex());

            let mut verifier = checksum.verifier();
            verifier.update(&data[1..]);
            assert!(!verifier.validate());
            assert_ne!(verifier.actual_hex(), checksum.expected_hex());
        }
    }

    fn client() -> Client {
        Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            false,
            Default::default(),
            std::num::NonZeroU16::new(300).unwrap(),
            std::num::NonZeroU64::new(1).unwrap(),
            [],
        )
        .unwrap()
    }

    const BINSTALL: &str = "https://github.com/cargo-bins/cargo-binstall";

    /// Its releases have no checksum files, but signatures, which stand in
    /// for them here.
    static SIG: [Sidecar; 2] = [
        Sidecar {
            suffix: ".sha256",
            algorithm: Algorithm::Sha256,
            probe: true,
        },
        Sidecar {
            suffix: ".sig",
            algorithm: Algorithm::Sha256,
            probe: false,
        },
    ];

    #[tokio::test]
    async fn listed_on_github_page() {
        let client = client();
        let gh_api_client = GhApiClient::new(client.clone(), None);
        let data = Data::new(
            "cargo-binstall".into(),
            "1.28.0".into(),
            Some(BINSTALL.into()),
        );
        let url = Url::parse(&format!(
            "{BINSTALL}/releases/download/v1.28.0/cargo-binstall-x86_64-unknown-linux-musl.tgz"
        ))
        .unwrap();
        let artifact = GhReleaseArtifact::try_extract_from_url(&url).unwrap();

        let listed = listed_on_github(&SIG, &client, &gh_api_client, &data, &artifact, false)
            .await
            .unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|(sidecar, artifact_url)| (sidecar.suffix, artifact_url.is_some()))
                .collect::<Vec<_>>(),
            [(".sig", false)]
        );

        // None of the real ones is listed, so none is asked for, and the
        // page is fetched once.
        assert!(
            find(&client, &gh_api_client, &data, &url, false)
                .await
                .is_none()
        );
        assert_eq!(data.repo_cache.release_assets_pages_len(), 1);

        // An asset the page doesn't list: it can't tell.
        let missing = Url::parse(&format!(
            "{BINSTALL}/releases/download/v1.28.0/cargo-binstall-no-such-target.tgz"
        ))
        .unwrap();
        let artifact = GhReleaseArtifact::try_extract_from_url(&missing).unwrap();
        assert!(
            listed_on_github(&SIG, &client, &gh_api_client, &data, &artifact, false)
                .await
                .is_none()
        );
    }

    #[test]
    fn file_names() {
        let name = |url: &str| file_name(&Url::parse(url).unwrap());
        assert_eq!(
            name("https://github.com/o/r/releases/download/v1/a%2Bb.tar.gz").as_deref(),
            Some("a+b.tar.gz")
        );
        assert_eq!(name("https://example.com/a.tgz?raw=1"), None);
        assert_eq!(name("https://example.com/dir/"), None);
        assert_eq!(
            name("https://sourceforge.net/projects/a/files/binaries/v1/a.tgz/download"),
            None
        );
        assert_eq!(
            with_suffix(
                &Url::parse("https://example.com/a%2Bb.tgz").unwrap(),
                ".sha256"
            )
            .as_str(),
            "https://example.com/a%2Bb.tgz.sha256"
        );
    }
}
