use std::borrow::Cow;

use base16::{decode as decode_base16, encode_lower as encode_base16};
use cargo_toml::Manifest;
use compact_str::{format_compact, CompactString, ToCompactString};
use leon::{Template, Values};
use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Error as JsonError;
use sha2::{Digest, Sha256};
use tracing::debug;

use crate::{
    drivers::registry::{visitor::ManifestVisitor, RegistryError},
    errors::BinstallError,
    helpers::{
        bytes::Bytes,
        download::{DataVerifier, Download},
        remote::{Client, Url},
    },
    manifests::cargo_toml_binstall::{Meta, TarBasedFmt},
};

#[derive(Deserialize)]
pub(super) struct RegistryConfig {
    pub(super) dl: CompactString,
}

struct Sha256Digest(Sha256);

impl Default for Sha256Digest {
    fn default() -> Self {
        Sha256Digest(Sha256::new())
    }
}

impl DataVerifier for Sha256Digest {
    fn update(&mut self, data: &Bytes) {
        self.0.update(data);
    }
}

pub(super) async fn parse_manifest(
    client: Client,
    crate_name: &str,
    crate_url: Url,
    MatchedVersion { version, cksum }: MatchedVersion,
) -> Result<Manifest<Meta>, BinstallError> {
    debug!("Fetching crate from: {crate_url} and extracting Cargo.toml from it");

    let mut manifest_visitor = ManifestVisitor::new(format!("{crate_name}-{version}").into());

    let checksum = decode_base16(cksum.as_bytes()).map_err(RegistryError::from)?;
    let mut sha256_digest = Sha256Digest::default();

    Download::new_with_data_verifier(client, crate_url, &mut sha256_digest)
        .and_visit_tar(TarBasedFmt::Tgz, &mut manifest_visitor)
        .await?;

    let digest_checksum = sha256_digest.0.finalize();

    if digest_checksum.as_slice() != checksum.as_slice() {
        Err(RegistryError::UnmatchedChecksum {
            expected: cksum,
            actual: encode_base16(digest_checksum.as_slice()),
        }
        .into())
    } else {
        manifest_visitor.load_manifest()
    }
}

/// Return components of crate prefix
pub(super) fn crate_prefix_components(
    crate_name: &str,
) -> Result<(CompactString, Option<CompactString>), RegistryError> {
    let mut chars = crate_name.chars();

    match (chars.next(), chars.next(), chars.next(), chars.next()) {
        (None, None, None, None) => Err(RegistryError::NotFound(crate_name.into())),
        (Some(_), None, None, None) => Ok((CompactString::new("1"), None)),
        (Some(_), Some(_), None, None) => Ok((CompactString::new("2"), None)),
        (Some(ch), Some(_), Some(_), None) => Ok((
            CompactString::new("3"),
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
    MatchedVersion { version, cksum }: &MatchedVersion,
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
            crate_lowerprefix: String,
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
    vers: CompactString,
    yanked: bool,
    cksum: String,
}

pub(super) struct MatchedVersion {
    pub(super) version: CompactString,
    /// sha256 checksum encoded in base16
    pub(super) cksum: String,
}

impl MatchedVersion {
    pub(super) fn find(
        it: &mut dyn Iterator<Item = Result<RegistryIndexEntry, JsonError>>,
        version_req: &VersionReq,
    ) -> Result<Self, BinstallError> {
        let mut ret = Option::<(Self, Version)>::None;

        for res in it {
            let entry = res.map_err(RegistryError::from)?;

            if entry.yanked {
                continue;
            }

            let num = entry.vers;

            // Parse out version
            let Ok(ver) = Version::parse(&num) else { continue };

            // Filter by version match
            if !version_req.matches(&ver) {
                continue;
            }

            let matched = Self {
                version: num,
                cksum: entry.cksum,
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
            .ok_or_else(|| BinstallError::VersionMismatch {
                req: version_req.clone(),
            })
    }
}
