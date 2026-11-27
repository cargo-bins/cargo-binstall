//! Find a package from the `dist-manifest.json` that
//! [dist](https://github.com/axodotdev/cargo-dist) uploads to each release,
//! for a release on the simple host of its `simple-download-url`.
//!
//! The manifest names the exact archive for every target, along with its
//! sha256 checksum, so it is all that needs to be found on a host whose
//! files can't be listed.
//!
//! Only the few fields needed are parsed, and unknown fields and kinds are
//! ignored, so that new versions of dist don't break this.

use std::collections::BTreeMap;

use binstalk_downloader::{download::DataVerifier, remote::StatusCode};
use binstalk_git_repo_api::gh_api_client::GhReleaseArtifactUrl;
use bytes::{Bytes, BytesMut};
use compact_str::CompactString;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{FetchError, common::*, fetch_context::Sha256Id};

/// The file dist uploads to the release.
pub(super) const DIST_MANIFEST: &str = "dist-manifest.json";

/// Upper bound on the size of a manifest worth reading: dist's own,
/// with 22 artifacts, is 32KiB.
pub(super) const MAX_MANIFEST_SIZE: usize = 8 * 1024 * 1024;

/// Upper bound on the size of a `.sha256` checksum file worth reading.
pub(super) const MAX_CHECKSUM_FILE_SIZE: usize = 4096;

#[derive(Debug, Deserialize)]
pub(super) struct DistManifest {
    #[serde(default)]
    releases: Vec<Release>,
    /// Key: artifact id
    #[serde(default)]
    artifacts: BTreeMap<CompactString, Artifact>,
}

#[derive(Debug, Deserialize)]
struct Release {
    app_name: CompactString,
    app_version: CompactString,
    /// Ids of the artifacts of this release.
    #[serde(default)]
    artifacts: Vec<CompactString>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    /// Filename of the artifact in the release.
    name: Option<CompactString>,
    kind: CompactString,
    #[serde(default)]
    target_triples: Vec<CompactString>,
    /// Id of the artifact holding the checksum of this one.
    checksum: Option<CompactString>,
    /// Inline checksums, key: algorithm.
    #[serde(default)]
    checksums: BTreeMap<CompactString, CompactString>,
}

/// The archive of the package for a target, found in [`DistManifest`].
#[derive(Debug, Eq, PartialEq)]
pub(super) struct DistArtifact<'a> {
    /// Filename of the archive, in the same release as the manifest.
    pub(super) filename: &'a str,
    pub(super) pkg_fmt: PkgFmt,
    /// The extension of `filename` that `pkg_fmt` was matched from.
    pub(super) archive_suffix: &'static str,
    pub(super) sha256: Option<Sha256Source<'a>>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Sha256Source<'a> {
    Inline([u8; 32]),
    /// Filename of a file whose first word is the hex encoded checksum,
    /// in the same release as the manifest.
    File(&'a str),
}

/// Whether `filename` can safely be joined to the manifest's url, as a file
/// next to it.
fn is_plain_filename(filename: &str) -> bool {
    !filename.is_empty()
        && filename != "."
        && filename != ".."
        && !filename.contains(['/', '\\', '?', '#', '%'])
}

fn decode_sha256(hex: &str) -> Option<[u8; 32]> {
    let mut sha256 = [0; 32];
    (hex.len() == 64 && base16::decode_slice(hex, &mut sha256).ok()? == 32).then_some(sha256)
}

impl DistManifest {
    pub(super) fn parse(json: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(json)
    }

    /// The archive of `name`@`version` for `target`.
    ///
    /// `pkg_fmt` is the one the crate specified, if any: an archive of
    /// another format is skipped.
    pub(super) fn find_artifact(
        &self,
        name: &str,
        version: &str,
        target: &str,
        pkg_fmt: Option<PkgFmt>,
    ) -> Option<DistArtifact<'_>> {
        let is_windows = target.contains("windows");
        let release = self
            .releases
            .iter()
            .find(|release| release.app_name == name && release.app_version == version)?;

        release
            .artifacts
            .iter()
            .filter_map(|id| self.artifacts.get(id))
            .filter(|artifact| {
                artifact.kind == "executable-zip"
                    && artifact.target_triples.iter().any(|t| t == target)
            })
            .find_map(|artifact| {
                let filename = artifact.name.as_deref().filter(|f| is_plain_filename(f))?;

                let found_fmt = PkgFmt::guess_pkg_format(filename)?;
                if pkg_fmt.is_some_and(|pkg_fmt| pkg_fmt != found_fmt) {
                    return None;
                }
                // The longest matching extension, e.g. `.tar.gz` and not `.gz`.
                let archive_suffix = found_fmt
                    .extensions(is_windows)
                    .iter()
                    .copied()
                    .filter(|ext| filename.ends_with(ext))
                    .max_by_key(|ext| ext.len())?;

                let sha256 = if let Some(hex) = artifact.checksums.get("sha256") {
                    match decode_sha256(hex) {
                        Some(sha256) => Some(Sha256Source::Inline(sha256)),
                        None => {
                            warn!(
                                "Invalid sha256 checksum {hex:?} of {filename} in {DIST_MANIFEST}"
                            );
                            return None;
                        }
                    }
                } else {
                    artifact
                        .checksum
                        .as_ref()
                        .and_then(|id| self.artifacts.get(id))
                        .filter(|checksum| checksum.kind == "checksum")
                        .and_then(|checksum| checksum.name.as_deref())
                        .filter(|f| is_plain_filename(f) && f.ends_with(".sha256"))
                        .map(Sha256Source::File)
                };

                Some(DistArtifact {
                    filename,
                    pkg_fmt: found_fmt,
                    archive_suffix,
                    sha256,
                })
            })
    }
}

/// Read the body of `url`, a release asset, if it exists.
///
/// * `artifact_url` - Its API url, to download it with, for a private
///   repository.
/// * `got` - The response to a GET of it already sent, if any.
///
/// Fails if the body is larger than `max_size`.
pub(super) async fn download_release_asset(
    client: &Client,
    gh_api_client: &GhApiClient,
    url: &Url,
    artifact_url: Option<GhReleaseArtifactUrl>,
    got: Option<GetResponse>,
    max_size: usize,
) -> Result<Option<Bytes>, FetchError> {
    let too_large =
        || FetchError::DistManifest(format!("{url} is larger than {max_size} bytes").into());

    if let Some(artifact_url) = artifact_url {
        let bytes = gh_api_client
            .download_artifact(artifact_url)
            .await?
            .into_bytes()
            .await?;
        return if bytes.len() <= max_size {
            Ok(Some(bytes))
        } else {
            Err(too_large())
        };
    }

    let mut response = match got {
        Some(got) => got.into_response(),
        None => {
            let response = client.get(url.clone()).send(false).await?;
            if response.status() == StatusCode::NOT_FOUND {
                return Ok(None);
            }
            response.error_for_status_ref()?;
            response
        }
    };

    let mut body = BytesMut::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > max_size {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body.freeze()))
}

/// Parse a `.sha256` file as dist writes it: the hex encoded checksum,
/// optionally followed by whitespace and the filename.
pub(super) fn parse_sha256_file(contents: &[u8]) -> Option<[u8; 32]> {
    let contents = std::str::from_utf8(contents).ok()?;
    decode_sha256(contents.split_whitespace().next()?)
}

/// The sha256 checksum a package found in a `dist-manifest.json` must have.
#[derive(Debug)]
pub(super) enum ExpectedSha256 {
    /// Listed in the manifest.
    Inline([u8; 32]),
    /// In the checksum file at `url`, read in the background while the
    /// package downloads. `None` if it can't be read.
    File {
        url: Url,
        task: AutoAbortJoinHandle<Result<Option<[u8; 32]>, FetchError>>,
    },
}

impl ExpectedSha256 {
    /// What the package is checked against, see
    /// [`PackageId::sha256`](crate::fetch_context::PackageId::sha256).
    pub(super) fn id(&self) -> Sha256Id {
        match self {
            Self::Inline(sha256) => Sha256Id::Inline(*sha256),
            Self::File { url, .. } => Sha256Id::File(url.clone()),
        }
    }

    /// Wait for the checksum, `None` if its file can't be read.
    pub(super) async fn get(self) -> Result<Option<[u8; 32]>, FetchError> {
        match self {
            Self::Inline(sha256) => Ok(Some(sha256)),
            Self::File { task, .. } => task.flattened_join().await,
        }
    }
}

/// Hashes the data downloaded, to compare it with an [`ExpectedSha256`]
/// once that is known.
pub(super) struct Sha256Verifier {
    state: Option<Sha256>,
    actual: Option<[u8; 32]>,
    expected: Option<[u8; 32]>,
}

impl Sha256Verifier {
    pub(super) fn new() -> Self {
        Self {
            state: Some(Sha256::new()),
            actual: None,
            expected: None,
        }
    }

    /// Set the checksum [`DataVerifier::validate`] checks against.
    pub(super) fn expect(&mut self, sha256: [u8; 32]) {
        self.expected = Some(sha256);
    }

    pub(super) fn expected_hex(&self) -> Option<String> {
        self.expected.map(|sha256| base16::encode_lower(&sha256))
    }

    pub(super) fn actual_hex(&self) -> Option<String> {
        self.actual.map(|sha256| base16::encode_lower(&sha256))
    }
}

impl DataVerifier for Sha256Verifier {
    fn update(&mut self, data: &Bytes) {
        if let Some(state) = &mut self.state {
            state.update(data);
        }
    }

    /// Fails if [`Sha256Verifier::expect`] wasn't called.
    fn validate(&mut self) -> bool {
        if let Some(state) = self.state.take() {
            self.actual = Some(state.finalize().into());
        }
        self.expected.is_some() && self.actual == self.expected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the `dist-manifest.json` of
    /// https://github.com/axodotdev/cargo-dist/releases/tag/v0.34.0, whose
    /// binary `dist` is named differently from the app `cargo-dist`, plus an
    /// app built for Windows as a zip and an older one with only a checksum
    /// file.
    const MANIFEST: &str = r#"{
  "dist_version": "0.34.0-prerelease.1",
  "announcement_tag": "v0.34.0",
  "releases": [
    {
      "app_name": "cargo-dist",
      "app_version": "0.34.0",
      "artifacts": [
        "source.tar.gz",
        "cargo-dist-installer.sh",
        "cargo-dist-x86_64-unknown-linux-gnu.tar.xz",
        "cargo-dist-x86_64-unknown-linux-gnu.tar.xz.sha256",
        "cargo-dist-x86_64-pc-windows-msvc.zip",
        "cargo-dist-aarch64-apple-darwin.tar.xz",
        "cargo-dist-aarch64-apple-darwin.tar.xz.sha256",
        "cargo-dist-x86_64-pc-windows-msvc-update",
        "nonexistent"
      ],
      "hosting": { "github": { "artifact_base_url": "https://github.com" } }
    },
    {
      "app_name": "other",
      "app_version": "1.0.0",
      "artifacts": ["other-x86_64-unknown-linux-gnu.tar.gz"]
    }
  ],
  "artifacts": {
    "source.tar.gz": {
      "name": "source.tar.gz",
      "kind": "source-tarball",
      "checksum": "source.tar.gz.sha256"
    },
    "cargo-dist-installer.sh": {
      "name": "cargo-dist-installer.sh",
      "kind": "installer",
      "target_triples": ["x86_64-unknown-linux-gnu"]
    },
    "cargo-dist-x86_64-unknown-linux-gnu.tar.xz": {
      "name": "cargo-dist-x86_64-unknown-linux-gnu.tar.xz",
      "kind": "executable-zip",
      "target_triples": ["x86_64-unknown-linux-gnu"],
      "assets": [
        { "id": "cargo-dist-x86_64-unknown-linux-gnu-exe-dist", "name": "dist", "path": "dist", "kind": "executable" }
      ],
      "checksum": "cargo-dist-x86_64-unknown-linux-gnu.tar.xz.sha256",
      "checksums": { "sha256": "7fee44ff3ff71ed0064b32d4ec62fee6b58bd37c5887b12a86aedddc5af0c7b5" }
    },
    "cargo-dist-x86_64-unknown-linux-gnu.tar.xz.sha256": {
      "name": "cargo-dist-x86_64-unknown-linux-gnu.tar.xz.sha256",
      "kind": "checksum",
      "target_triples": ["x86_64-unknown-linux-gnu"]
    },
    "cargo-dist-x86_64-pc-windows-msvc.zip": {
      "name": "cargo-dist-x86_64-pc-windows-msvc.zip",
      "kind": "executable-zip",
      "target_triples": ["x86_64-pc-windows-msvc"]
    },
    "cargo-dist-x86_64-pc-windows-msvc-update": {
      "name": "cargo-dist-x86_64-pc-windows-msvc-update",
      "kind": "updater",
      "target_triples": ["x86_64-pc-windows-msvc"]
    },
    "cargo-dist-aarch64-apple-darwin.tar.xz": {
      "name": "cargo-dist-aarch64-apple-darwin.tar.xz",
      "kind": "executable-zip",
      "target_triples": ["aarch64-apple-darwin"],
      "checksum": "cargo-dist-aarch64-apple-darwin.tar.xz.sha256"
    },
    "cargo-dist-aarch64-apple-darwin.tar.xz.sha256": {
      "name": "cargo-dist-aarch64-apple-darwin.tar.xz.sha256",
      "kind": "checksum"
    },
    "other-x86_64-unknown-linux-gnu.tar.gz": {
      "name": "../other-x86_64-unknown-linux-gnu.tar.gz",
      "kind": "executable-zip",
      "target_triples": ["x86_64-unknown-linux-gnu"]
    }
  },
  "systems": {},
  "ci": { "github": { "artifacts_matrix": { "include": [] } } }
}"#;

    fn manifest() -> DistManifest {
        DistManifest::parse(MANIFEST.as_bytes()).unwrap()
    }

    #[test]
    fn inline_sha256() {
        assert_eq!(
            manifest().find_artifact("cargo-dist", "0.34.0", "x86_64-unknown-linux-gnu", None),
            Some(DistArtifact {
                filename: "cargo-dist-x86_64-unknown-linux-gnu.tar.xz",
                pkg_fmt: PkgFmt::Txz,
                archive_suffix: ".tar.xz",
                sha256: Some(Sha256Source::Inline(
                    decode_sha256(
                        "7fee44ff3ff71ed0064b32d4ec62fee6b58bd37c5887b12a86aedddc5af0c7b5"
                    )
                    .unwrap()
                )),
            })
        );
    }

    #[test]
    fn checksum_file() {
        assert_eq!(
            manifest().find_artifact("cargo-dist", "0.34.0", "aarch64-apple-darwin", None),
            Some(DistArtifact {
                filename: "cargo-dist-aarch64-apple-darwin.tar.xz",
                pkg_fmt: PkgFmt::Txz,
                archive_suffix: ".tar.xz",
                sha256: Some(Sha256Source::File(
                    "cargo-dist-aarch64-apple-darwin.tar.xz.sha256"
                )),
            })
        );
    }

    #[test]
    fn windows_zip_without_checksum() {
        assert_eq!(
            manifest().find_artifact("cargo-dist", "0.34.0", "x86_64-pc-windows-msvc", None),
            Some(DistArtifact {
                filename: "cargo-dist-x86_64-pc-windows-msvc.zip",
                pkg_fmt: PkgFmt::Zip,
                archive_suffix: ".zip",
                sha256: None,
            })
        );
    }

    #[test]
    fn not_found() {
        let manifest = manifest();
        // Another version
        assert_eq!(
            manifest.find_artifact("cargo-dist", "0.33.0", "x86_64-unknown-linux-gnu", None),
            None
        );
        // Another app
        assert_eq!(
            manifest.find_artifact("dist", "0.34.0", "x86_64-unknown-linux-gnu", None),
            None
        );
        // A target without an archive
        assert_eq!(
            manifest.find_artifact("cargo-dist", "0.34.0", "x86_64-unknown-linux-musl", None),
            None
        );
        // pkg-fmt specified by the crate doesn't match
        assert_eq!(
            manifest.find_artifact(
                "cargo-dist",
                "0.34.0",
                "x86_64-unknown-linux-gnu",
                Some(PkgFmt::Tgz)
            ),
            None
        );
        // Filename that is not a file next to the manifest
        assert_eq!(
            manifest.find_artifact("other", "1.0.0", "x86_64-unknown-linux-gnu", None),
            None
        );
    }

    #[test]
    fn minimal_and_invalid() {
        assert!(DistManifest::parse(b"{}").is_ok());
        assert!(DistManifest::parse(b"not json").is_err());
        assert!(DistManifest::parse(br#"{"releases": 1}"#).is_err());
    }

    #[test]
    fn sha256_file() {
        let hex = "7fee44ff3ff71ed0064b32d4ec62fee6b58bd37c5887b12a86aedddc5af0c7b5";
        let expected = decode_sha256(hex);
        assert!(expected.is_some());
        assert_eq!(
            parse_sha256_file(format!("{hex}  dist.tar.xz\n").as_bytes()),
            expected
        );
        assert_eq!(parse_sha256_file(format!("{hex}\n").as_bytes()), expected);
        assert_eq!(parse_sha256_file(&hex.as_bytes()[1..]), None);
        assert_eq!(parse_sha256_file(b""), None);
    }

    #[test]
    fn sha256_verifier() {
        let data = Bytes::from_static(b"hello");
        let expected: [u8; 32] = Sha256::digest(&data).into();

        let mut verifier = Sha256Verifier::new();
        verifier.update(&data.slice(..2));
        verifier.update(&data.slice(2..));
        verifier.expect(expected);
        assert!(verifier.validate());

        let mut verifier = Sha256Verifier::new();
        verifier.update(&Bytes::from_static(b"world"));
        verifier.expect(expected);
        assert!(!verifier.validate());
        assert_ne!(verifier.actual_hex(), verifier.expected_hex());

        // Nothing to check against.
        let mut verifier = Sha256Verifier::new();
        verifier.update(&data);
        assert!(!verifier.validate());
    }
}
