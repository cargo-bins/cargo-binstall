use std::borrow::Cow;

use base16::{decode as decode_base16, encode_lower as encode_base16};
use binstalk_downloader::{
    bytes::Bytes,
    download::{DataVerifier, Download},
    remote::{Client, RequestBuilder, Url},
};
use binstalk_types::cargo_toml_binstall::{Meta, TarBasedFmt};
use cargo_toml_workspace::cargo_toml::Manifest;
use compact_str::{format_compact, CompactString, ToCompactString};
use jiff::Timestamp;
use leon::{Template, Values};
use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Error as JsonError;
use sha2::{Digest, Sha256};
use tracing::{debug, info, instrument};

use crate::{publish_age::is_too_new, visitor::ManifestVisitor, RegistryAuth, RegistryError};

#[derive(Clone, Debug, Deserialize)]
pub(super) struct RegistryConfig {
    pub(super) dl: CompactString,
    #[serde(rename = "auth-required", default)]
    pub(super) auth_required: bool,
}

struct Sha256Digest {
    expected: Vec<u8>,
    actual: Option<Vec<u8>>,
    state: Option<Sha256>,
}

impl Sha256Digest {
    fn new(checksum: Vec<u8>) -> Self {
        Self {
            expected: checksum,
            actual: None,
            state: Some(Sha256::new()),
        }
    }
}

impl DataVerifier for Sha256Digest {
    fn update(&mut self, data: &Bytes) {
        if let Some(ref mut state) = &mut self.state {
            state.update(data);
        }
    }

    fn validate(&mut self) -> bool {
        if let Some(state) = self.state.take() {
            self.actual = Some(state.finalize().to_vec());
        }

        self.actual.as_ref().unwrap() == &self.expected
    }
}

#[instrument(
    skip(client, crate_url),
    fields(
        crate_url = format_args!("{crate_url}"),
    ),
)]
pub(super) async fn parse_manifest(
    client: Client,
    crate_name: &str,
    crate_url: Url,
    MatchedVersion { version, cksum, .. }: MatchedVersion,
    auth: Option<&RegistryAuth>,
) -> Result<Manifest<Meta>, RegistryError> {
    debug!("Fetching crate from: {crate_url} and extracting Cargo.toml from it");

    let mut manifest_visitor = ManifestVisitor::new(format!("{crate_name}-{version}").into());

    let checksum = decode_base16(cksum.as_bytes()).map_err(RegistryError::from)?;
    let mut digest = Sha256Digest::new(checksum);

    Download::from_response_with_data_verifier(
        apply_auth(client.get(crate_url), auth).send(true).await?,
        &mut digest,
    )
    .and_visit_tar(TarBasedFmt::Tgz, &mut manifest_visitor)
    .await?;

    if !digest.validate() {
        Err(RegistryError::UnmatchedChecksum {
            expected: encode_base16(digest.expected.as_slice()).into(),
            actual: encode_base16(digest.actual.unwrap().as_slice()).into(),
        })
    } else {
        manifest_visitor.load_manifest()
    }
}

pub(super) fn apply_auth(request: RequestBuilder, auth: Option<&RegistryAuth>) -> RequestBuilder {
    if let Some(auth) = auth {
        request.header("Authorization", auth.token())
    } else {
        request
    }
}

/// Return components of crate prefix
pub(super) fn crate_prefix_components(
    crate_name: &str,
) -> Result<(CompactString, Option<CompactString>), RegistryError> {
    let mut chars = crate_name.chars();

    match (chars.next(), chars.next(), chars.next(), chars.next()) {
        (None, None, None, None) => Err(RegistryError::NotFound(crate_name.into())),
        (Some(_), None, None, None) => Ok((CompactString::const_new("1"), None)),
        (Some(_), Some(_), None, None) => Ok((CompactString::const_new("2"), None)),
        (Some(ch), Some(_), Some(_), None) => Ok((
            CompactString::const_new("3"),
            Some(ch.to_lowercase().to_compact_string()),
        )),
        (Some(a), Some(b), Some(c), Some(d)) => Ok((
            format_compact!("{}{}", a.to_lowercase(), b.to_lowercase()),
            Some(format_compact!("{}{}", c.to_lowercase(), d.to_lowercase())),
        )),
        _ => unreachable!(),
    }
}

pub(super) fn render_dl_template(
    dl_template: &str,
    crate_name: &str,
    (c1, c2): &(CompactString, Option<CompactString>),
    MatchedVersion { version, cksum, .. }: &MatchedVersion,
) -> Result<String, RegistryError> {
    let template = Template::parse(dl_template)?;
    if template.keys().next().is_some() {
        let mut crate_prefix = c1.clone();
        if let Some(c2) = c2 {
            crate_prefix.push('/');
            crate_prefix.push_str(c2);
        }

        struct Context<'a> {
            crate_name: &'a str,
            crate_prefix: CompactString,
            crate_lowerprefix: CompactString,
            version: &'a str,
            cksum: &'a str,
        }
        impl Values for Context<'_> {
            fn get_value(&self, key: &str) -> Option<Cow<'_, str>> {
                match key {
                    "crate" => Some(Cow::Borrowed(self.crate_name)),
                    "version" => Some(Cow::Borrowed(self.version)),
                    "prefix" => Some(Cow::Borrowed(&self.crate_prefix)),
                    "lowerprefix" => Some(Cow::Borrowed(&self.crate_lowerprefix)),
                    "sha256-checksum" => Some(Cow::Borrowed(self.cksum)),
                    _ => None,
                }
            }
        }
        Ok(template.render(&Context {
            crate_name,
            crate_lowerprefix: crate_prefix.to_lowercase(),
            crate_prefix,
            version,
            cksum,
        })?)
    } else {
        Ok(format!("{dl_template}/{crate_name}/{version}/download"))
    }
}

#[derive(Deserialize)]
pub(super) struct RegistryIndexEntry {
    pub(super) vers: CompactString,
    pub(super) yanked: bool,
    pub(super) cksum: String,
    pub(super) pubtime: Option<CompactString>,
}

pub(super) struct MatchedVersion {
    pub(super) version: CompactString,
    /// sha256 checksum encoded in base16
    pub(super) cksum: String,
    /// Publish time (RFC 3339), if the registry index records it
    pub(super) pubtime: Option<CompactString>,
}

impl MatchedVersion {
    /// Find the highest version matching `version_req` that is not yanked
    /// and, if `cutoff` is set, was not published after `cutoff`.
    pub(super) fn find(
        it: &mut dyn Iterator<Item = Result<RegistryIndexEntry, JsonError>>,
        version_req: &VersionReq,
        cutoff: Option<Timestamp>,
    ) -> Result<Self, RegistryError> {
        let mut ret = Option::<(Self, Version)>::None;
        // Highest matching version skipped for being too new
        let mut too_new = Option::<(Self, Version)>::None;

        for res in it {
            let entry = res.map_err(RegistryError::from)?;

            if entry.yanked {
                continue;
            }

            let num = entry.vers;

            // Parse out version
            let Ok(ver) = Version::parse(&num) else {
                continue;
            };

            // Filter by version match
            if !version_req.matches(&ver) {
                continue;
            }

            let is_too_new =
                cutoff.is_some_and(|cutoff| is_too_new(entry.pubtime.as_deref(), cutoff));

            let matched = Self {
                version: num,
                cksum: entry.cksum,
                pubtime: entry.pubtime,
            };

            let slot = if is_too_new { &mut too_new } else { &mut ret };
            if slot.as_ref().map_or(true, |(_, max_ver)| ver > *max_ver) {
                *slot = Some((matched, ver));
            }
        }

        match (ret, too_new) {
            (Some((matched, ver)), too_new) => {
                if let Some((too_new, too_new_ver)) = too_new {
                    if too_new_ver > ver {
                        info!(
                            "Skipping version {} published at {}, it is newer than min-publish-age",
                            too_new.version,
                            too_new.pubtime.as_deref().unwrap_or_default(),
                        );
                    }
                }
                Ok(matched)
            }
            (None, Some((too_new, _))) => Err(RegistryError::TooNew {
                req: version_req.clone(),
                version: too_new.version,
                pubtime: too_new.pubtime.unwrap_or_default(),
            }),
            (None, None) => Err(RegistryError::VersionMismatch {
                req: version_req.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod test {
    use serde_json::Deserializer as JsonDeserializer;

    use super::*;

    #[test]
    fn test_matched_version_pubtime() {
        let index = br#"{"name":"foo","vers":"0.1.0","deps":[],"cksum":"00","features":{},"yanked":false,"pubtime":"2018-06-07T16:11:07Z"}
{"name":"foo","vers":"0.2.0","deps":[],"cksum":"11","features":{},"yanked":false}
"#;
        let find = |req| {
            MatchedVersion::find(
                &mut JsonDeserializer::from_slice(index).into_iter(),
                &VersionReq::parse(req).unwrap(),
                None,
            )
            .unwrap()
        };

        assert_eq!(
            find("=0.1.0").pubtime.as_deref(),
            Some("2018-06-07T16:11:07Z")
        );
        // Older registries may not record a publish time.
        assert_eq!(find("=0.2.0").pubtime, None);
    }

    #[test]
    fn test_matched_version_min_publish_age() {
        let index = br#"{"name":"foo","vers":"1.0.0","deps":[],"cksum":"00","features":{},"yanked":false,"pubtime":"2026-10-01T00:00:00Z"}
{"name":"foo","vers":"1.1.0","deps":[],"cksum":"11","features":{},"yanked":false,"pubtime":"2026-11-01T00:00:00Z"}
{"name":"foo","vers":"1.2.0","deps":[],"cksum":"22","features":{},"yanked":false,"pubtime":"2026-11-10T00:00:00Z"}
{"name":"foo","vers":"1.3.0","deps":[],"cksum":"33","features":{},"yanked":false,"pubtime":"2026-11-13T00:00:00Z"}
{"name":"foo","vers":"2.0.0","deps":[],"cksum":"44","features":{},"yanked":false,"pubtime":"2026-11-13T12:00:00Z"}
{"name":"bar","vers":"0.1.0","deps":[],"cksum":"55","features":{},"yanked":false}
"#;
        // 7 days before 2026-11-14
        let cutoff = Some("2026-11-07T00:00:00Z".parse().unwrap());
        let find = |req| {
            MatchedVersion::find(
                &mut JsonDeserializer::from_slice(index).into_iter(),
                &VersionReq::parse(req).unwrap(),
                cutoff,
            )
        };

        // Too new versions are skipped in favour of older matching ones.
        assert_eq!(find("*").unwrap().version, "1.1.0");
        assert_eq!(find("^1").unwrap().version, "1.1.0");
        assert_eq!(find("=1.0.0").unwrap().version, "1.0.0");
        // Versions without pubtime are not affected.
        assert_eq!(find("=0.1.0").unwrap().version, "0.1.0");

        // Error out when every matching version is too new, even if it is
        // requested explicitly, reporting the latest one.
        match find("^1.2").err().unwrap() {
            RegistryError::TooNew {
                version, pubtime, ..
            } => {
                assert_eq!(version, "1.3.0");
                assert_eq!(pubtime, "2026-11-13T00:00:00Z");
            }
            err => panic!("unexpected error {err}"),
        }
        assert!(matches!(
            find("=2.0.0").err().unwrap(),
            RegistryError::TooNew { version, .. } if version == "2.0.0"
        ));
        assert!(matches!(
            find("=3.0.0").err().unwrap(),
            RegistryError::VersionMismatch { .. }
        ));
    }
}
