//! Common structure for crate information for post-install manifests.

use std::{borrow, cmp, hash, sync::LazyLock};

use compact_str::CompactString;
use maybe_owned::MaybeOwned;
use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::cargo_toml_binstall::PkgFmt;

pub fn cratesio_url() -> &'static Url {
    static CRATESIO: LazyLock<Url> =
        LazyLock::new(|| Url::parse("https://github.com/rust-lang/crates.io-index").unwrap());

    &CRATESIO
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrateInfo {
    pub name: CompactString,
    pub version_req: CompactString,
    pub current_version: Version,
    pub source: CrateSource,
    pub target: CompactString,
    pub bins: Vec<CompactString>,
    /// How the package was found on `target`, when it came from a
    /// `pkg-url`, so that updating it can try that first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pkg_url_hint: Option<PkgUrlHint>,
}

/// The `pkg-url` that a crate's package was found with, a default one or
/// the crate's own.
///
/// Binstall otherwise probes every `pkg-url` x `pkg-fmt` x archive suffix
/// combination on each install, which is a lot of requests and GitHub API
/// usage for an update whose asset naming almost never changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkgUrlHint {
    /// The name `{ name }` was rendered with: the crate's, or its only
    /// binary's.
    pub name: CompactString,
    /// The template, in the same syntax as `pkg-url` in `Cargo.toml`.
    pub pkg_url: CompactString,
    pub pkg_fmt: PkgFmt,
    /// What else the URL was rendered with, so the hint renders to that one
    /// URL. `None` in a hint written before it was recorded: then every URL
    /// of `pkg_url` and `pkg_fmt` is tried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_with: Option<RenderedWith>,
}

/// The parts of a `pkg-url` that can render to more than one value for the
/// same target.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedWith {
    /// The archive suffix, e.g. `.tar.gz`. `None` if `pkg_url` has none of
    /// `{ format }`, `{ archive-format }` and `{ archive-suffix }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_suffix: Option<CompactString>,
    /// The other name `{ target-arch }` was rendered as, e.g. `amd64`.
    /// `None` for the architecture's own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_arch: Option<CompactString>,
}

impl borrow::Borrow<str> for CrateInfo {
    fn borrow(&self) -> &str {
        &self.name
    }
}

impl PartialEq for CrateInfo {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}
impl Eq for CrateInfo {}

impl PartialOrd for CrateInfo {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CrateInfo {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.name.cmp(&other.name)
    }
}

impl hash::Hash for CrateInfo {
    fn hash<H>(&self, state: &mut H)
    where
        H: hash::Hasher,
    {
        self.name.hash(state)
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub enum SourceType {
    Git,
    Path,
    Registry,
    Sparse,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CrateSource {
    pub source_type: SourceType,
    /// Records written by older versions may have credentials, see
    /// [`CrateSource::new`].
    #[serde(deserialize_with = "deserialize_url_without_credentials")]
    pub url: MaybeOwned<'static, Url>,
}

/// Drop the password, and the user name over HTTP(S), where it is often a
/// token. Over SSH the user name is the login (`git@`), needed to fetch and
/// not a secret.
fn strip_credentials(url: &mut Url) {
    // Both fail only for URLs that cannot have credentials.
    if matches!(url.scheme(), "http" | "https") {
        let _ = url.set_username("");
    }
    let _ = url.set_password(None);
}

fn deserialize_url_without_credentials<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<MaybeOwned<'static, Url>, D::Error> {
    let mut url = Url::deserialize(deserializer)?;
    strip_credentials(&mut url);
    Ok(MaybeOwned::Owned(url))
}

impl CrateSource {
    /// A source for the install records, without the password of `url`, nor
    /// its user name over HTTP(S): cargo keeps them in `.crates.toml` and
    /// `.crates2.json`, but they are often a token, which should not be
    /// stored in plain text. Over SSH the user name is the login (`git@`).
    pub fn new(source_type: SourceType, mut url: Url) -> Self {
        strip_credentials(&mut url);
        Self {
            source_type,
            url: MaybeOwned::Owned(url),
        }
    }

    pub fn cratesio_registry() -> CrateSource {
        Self {
            source_type: SourceType::Registry,
            url: MaybeOwned::Borrowed(cratesio_url()),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_crate_source_new_strips_credentials() {
        for (url, expected) in [
            (
                "https://user:secret@example.com/index.git",
                "https://example.com/index.git",
            ),
            (
                "https://ghp_token@github.com/owner/repo?tag=v1#abc",
                "https://github.com/owner/repo?tag=v1#abc",
            ),
            ("https://example.com/index/", "https://example.com/index/"),
            // The ssh login is kept.
            (
                "ssh://git:secret@github.com/owner/repo",
                "ssh://git@github.com/owner/repo",
            ),
        ] {
            let source = CrateSource::new(SourceType::Git, Url::parse(url).unwrap());
            assert_eq!(source.url.as_str(), expected);

            let json = format!(r#"{{"source_type":"Git","url":"{url}"}}"#);
            let source: CrateSource = serde_json::from_str(&json).unwrap();
            assert_eq!(source.url.as_str(), expected);
        }
    }

    #[test]
    fn test_pkg_url_hint_serde() {
        // Written before `rendered_with` was recorded.
        let old: PkgUrlHint = serde_json::from_str(
            r#"{"name":"a","pkg_url":"{ repo }/releases/download/v{ version }/{ name }-{ target }{ archive-suffix }","pkg_fmt":"tgz"}"#,
        )
        .unwrap();
        assert_eq!(old.rendered_with, None);
        assert!(
            !serde_json::to_string(&old)
                .unwrap()
                .contains("rendered_with")
        );

        let hint = PkgUrlHint {
            rendered_with: Some(RenderedWith {
                archive_suffix: Some(".tar.gz".into()),
                target_arch: Some("amd64".into()),
            }),
            ..old.clone()
        };
        let json = serde_json::to_string(&hint).unwrap();
        assert!(
            json.ends_with(
                r#""rendered_with":{"archive_suffix":".tar.gz","target_arch":"amd64"}}"#
            ),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<PkgUrlHint>(&json).unwrap(), hint);

        // The architecture's own name, and no archive suffix.
        let hint = PkgUrlHint {
            rendered_with: Some(RenderedWith::default()),
            ..old
        };
        let json = serde_json::to_string(&hint).unwrap();
        assert!(json.ends_with(r#""rendered_with":{}}"#), "{json}");
        assert_eq!(serde_json::from_str::<PkgUrlHint>(&json).unwrap(), hint);
    }
}
