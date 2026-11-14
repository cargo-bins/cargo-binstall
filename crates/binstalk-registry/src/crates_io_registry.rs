use std::time::Duration;

use binstalk_downloader::remote::{Client, Error as RemoteError, Url};
use binstalk_types::cargo_toml_binstall::Meta;
use cargo_toml_workspace::cargo_toml::Manifest;
use compact_str::{CompactString, ToCompactString};
use jiff::Timestamp;
use semver::{Comparator, Op as ComparatorOp, Version as SemVersion, VersionReq};
use serde::Deserialize;
use tracing::{debug, instrument};

use crate::{
    parse_manifest, publish_age::publish_cutoff, MatchedVersion, RegistryError, RegistryIndexEntry,
};

#[derive(Deserialize)]
struct Version {
    num: CompactString,
    yanked: bool,
    checksum: String,
    created_at: CompactString,
}

impl From<Version> for RegistryIndexEntry {
    fn from(version: Version) -> Self {
        Self {
            vers: version.num,
            yanked: version.yanked,
            cksum: version.checksum,
            // Truncate to whole seconds to match `pubtime` in the registry index
            pubtime: Some(
                version
                    .created_at
                    .parse::<Timestamp>()
                    .map(|created_at| {
                        created_at
                            .strftime("%Y-%m-%dT%H:%M:%SZ")
                            .to_compact_string()
                    })
                    .unwrap_or(version.created_at),
            ),
        }
    }
}

/// Fetch versions of the crate, or only the requested one if `version_req`
/// is an exact version.
async fn fetch_crate_cratesio_versions(
    client: &Client,
    mut url: Url,
    version_req: &VersionReq,
) -> Result<Vec<Version>, RemoteError> {
    #[derive(Deserialize)]
    struct CrateInfo {
        versions: Vec<Version>,
    }

    #[derive(Deserialize)]
    struct CrateVersion {
        version: Version,
    }

    // Fetch / update index
    debug!("Looking up crate information");

    match version_req.comparators.as_slice() {
        [Comparator {
            op: ComparatorOp::Exact,
            major,
            minor: Some(minor),
            patch: Some(patch),
            pre,
        }] => {
            let version = SemVersion {
                major: *major,
                minor: *minor,
                patch: *patch,
                pre: pre.clone(),
                build: Default::default(),
            }
            .to_compact_string();

            url.path_segments_mut().unwrap().push(&version);

            let crate_version: CrateVersion = client.get(url).send(true).await?.json().await?;
            Ok(vec![crate_version.version])
        }
        _ => {
            let crate_info: CrateInfo = client.get(url).send(true).await?.json().await?;
            Ok(crate_info.versions)
        }
    }
}

/// Find the crate by name, get its latest stable version matches `version_req`,
/// retrieve its Cargo.toml and infer all its bins.
///
/// Returns the manifest along with the publish time (RFC 3339) of the
/// matched version.
#[instrument(
    skip(client),
    fields(
        version_req = format_args!("{version_req}"),
    )
)]
pub async fn fetch_crate_cratesio_api(
    client: Client,
    name: &str,
    version_req: &VersionReq,
    min_publish_age: Option<Duration>,
) -> Result<(Manifest<Meta>, Option<CompactString>), RegistryError> {
    let url = Url::parse(&format!("https://crates.io/api/v1/crates/{name}"))?;

    let versions = fetch_crate_cratesio_versions(&client, url.clone(), version_req)
        .await
        .map_err(|e| match e {
            RemoteError::Http(e) if e.is_status() => RegistryError::NotFound(name.into()),
            e => e.into(),
        })?;

    let mut matched_version = MatchedVersion::find(
        &mut versions.into_iter().map(|version| Ok(version.into())),
        version_req,
        min_publish_age.and_then(|min_age| publish_cutoff(Timestamp::now(), min_age)),
    )?;
    let version = &matched_version.version;

    debug!("Found information for crate version: '{version}'");

    // Download crate to temporary dir (crates.io or git?)
    let mut crate_url = url;
    crate_url
        .path_segments_mut()
        .unwrap()
        .push(version)
        .push("download");

    let pubtime = matched_version.pubtime.take();
    let manifest = parse_manifest(client, name, crate_url, matched_version, None).await?;

    Ok((manifest, pubtime))
}
