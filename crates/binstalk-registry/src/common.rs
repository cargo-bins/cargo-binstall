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
use leon::{Template, Values};
use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Error as JsonError;
use sha2::{Digest, Sha256};
use tracing::{debug, instrument};

use binstalk_types::{
    errors::{bail, OptionExt, Result, ResultExt},
    message,
};

use crate::{not_found, visitor::ManifestVisitor, RegistryAuth};

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
) -> Result<Manifest<Meta>> {
    debug!("Fetching crate from: {crate_url} and extracting Cargo.toml from it");

    let mut manifest_visitor = ManifestVisitor::new(format!("{crate_name}-{version}").into());

    let checksum = decode_base16(cksum.as_bytes())
        .or_raise_err(message!("Failed to parse checksum encoded in hex"))?;
    let mut digest = Sha256Digest::new(checksum);

    Download::from_response_with_data_verifier(
        apply_auth(client.get(crate_url), auth).send(true).await?,
        &mut digest,
    )
    .and_visit_tar(TarBasedFmt::Tgz, &mut manifest_visitor)
    .await
    .or_raise_err(message!("Failed to download crate"))?;

    if !digest.validate() {
        bail!(message!(
            "Expected checksum `{}`, actual checksum `{}`",
            encode_base16(digest.expected.as_slice()),
            encode_base16(digest.actual.unwrap().as_slice())
        ))
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
) -> Result<(CompactString, Option<CompactString>)> {
    let mut chars = crate_name.chars();

    match (chars.next(), chars.next(), chars.next(), chars.next()) {
        (None, None, None, None) => bail!(not_found(crate_name)),
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
) -> Result<String> {
    let template =
        Template::parse(dl_template).or_raise_err(message!("Failed to parse dl config"))?;
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
        Ok(template
            .render(&Context {
                crate_name,
                crate_lowerprefix: crate_prefix.to_lowercase(),
                crate_prefix,
                version,
                cksum,
            })
            .or_raise_err(message!("Failed to render dl config"))?)
    } else {
        Ok(format!("{dl_template}/{crate_name}/{version}/download"))
    }
}

#[derive(Deserialize)]
pub(super) struct RegistryIndexEntry {
    vers: CompactString,
    yanked: bool,
    cksum: String,
    pubtime: Option<CompactString>,
}

pub(super) struct MatchedVersion {
    pub(super) version: CompactString,
    /// sha256 checksum encoded in base16
    pub(super) cksum: String,
    /// Publish time (RFC 3339), if the registry index records it
    pub(super) pubtime: Option<CompactString>,
}

impl MatchedVersion {
    pub(super) fn find(
        it: &mut dyn Iterator<Item = Result<RegistryIndexEntry, JsonError>>,
        version_req: &VersionReq,
    ) -> Result<Self> {
        let mut ret = Option::<(Self, Version)>::None;

        for res in it {
            let entry = res?;

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

            let matched = Self {
                version: num,
                cksum: entry.cksum,
                pubtime: entry.pubtime,
            };

            if let Some((_, max_ver)) = &ret {
                if ver > *max_ver {
                    ret = Some((matched, ver));
                }
            } else {
                ret = Some((matched, ver));
            }
        }

        ret.map(|(num, _)| num)
            .ok_or_raise(|| message!("no version matching requirement '{version_req}'"))
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
}
