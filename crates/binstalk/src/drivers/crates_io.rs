use std::path::PathBuf;

use cargo_toml::Manifest;
use compact_str::{CompactString, ToCompactString};
use semver::{Comparator, Op as ComparatorOp, Version as SemVersion, VersionReq};
use serde::Deserialize;
use tracing::debug;

use crate::{
    errors::{BinstallError, CratesIoApiError},
    helpers::{
        download::Download,
        remote::{Client, Url},
    },
    manifests::cargo_toml_binstall::{Meta, TarBasedFmt},
    ops::CratesIoRateLimit,
};

mod vfs;

mod visitor;
use visitor::ManifestVisitor;

async fn is_crate_yanked(
    client: &Client,
    name: &str,
    version: &str,
) -> Result<bool, BinstallError> {
    #[derive(Deserialize)]
    struct CrateInfo {
        version: Inner,
    }

    #[derive(Deserialize)]
    struct Inner {
        yanked: bool,
    }

    // Fetch / update index
    debug!("Looking up crate information");

    let response = client
        .get(Url::parse(&format!(
            "https://crates.io/api/v1/crates/{name}/{version}"
        ))?)
        .send(true)
        .await
        .map_err(|err| {
            BinstallError::CratesIoApi(Box::new(CratesIoApiError {
                crate_name: name.into(),
                err,
            }))
        })?;

    let info: CrateInfo = response.json().await?;

    Ok(info.version.yanked)
}

async fn fetch_crate_cratesio_version_matched(
    client: &Client,
    name: &str,
    version_req: &VersionReq,
) -> Result<CompactString, BinstallError> {
    #[derive(Deserialize)]
    struct CrateInfo {
        #[serde(rename = "crate")]
        inner: CrateInfoInner,
    }

    #[derive(Deserialize)]
    struct CrateInfoInner {
        max_stable_version: CompactString,
    }

    #[derive(Deserialize)]
    struct Versions {
        versions: Vec<Version>,
    }

    #[derive(Deserialize)]
    struct Version {
        num: CompactString,
        yanked: bool,
    }

    // Fetch / update index
    debug!("Looking up crate information");

    let response = client
        .get(Url::parse(&format!(
            "https://crates.io/api/v1/crates/{name}"
        ))?)
        .send(true)
        .await
        .map_err(|err| {
            BinstallError::CratesIoApi(Box::new(CratesIoApiError {
                crate_name: name.into(),
                err,
            }))
        })?;

    let version = if version_req == &VersionReq::STAR {
        let crate_info: CrateInfo = response.json().await?;
        crate_info.inner.max_stable_version
    } else {
        let response: Versions = response.json().await?;
        response
            .versions
            .into_iter()
            .filter_map(|item| {
                if !item.yanked {
                    // Remove leading `v` for git tags
                    let num = if let Some(num) = item.num.strip_prefix('v') {
                        num.into()
                    } else {
                        item.num
                    };

                    // Parse out version
                    let ver = semver::Version::parse(&num).ok()?;

                    // Filter by version match
                    version_req.matches(&ver).then_some((num, ver))
                } else {
                    None
                }
            })
            // Return highest version
            .max_by(|(_ver_str_x, ver_x), (_ver_str_y, ver_y)| ver_x.cmp(ver_y))
            .ok_or_else(|| BinstallError::VersionMismatch {
                req: version_req.clone(),
            })?
            .0
    };

    debug!("Found information for crate version: '{version}'");

    Ok(version)
}

/// Find the crate by name, get its latest stable version matches `version_req`,
/// retrieve its Cargo.toml and infer all its bins.
pub async fn fetch_crate_cratesio(
    client: Client,
    name: &str,
    version_req: &VersionReq,
    crates_io_rate_limit: &CratesIoRateLimit,
) -> Result<Manifest<Meta>, BinstallError> {
    // Wait until we can make another request to crates.io
    crates_io_rate_limit.tick().await;

    let version = match version_req.comparators.as_slice() {
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

            if is_crate_yanked(&client, name, &version).await? {
                return Err(BinstallError::VersionMismatch {
                    req: version_req.clone(),
                });
            }

            version
        }
        _ => fetch_crate_cratesio_version_matched(&client, name, version_req).await?,
    };

    // Download crate to temporary dir (crates.io or git?)
    let crate_url = format!("https://crates.io/api/v1/crates/{name}/{version}/download");

    debug!("Fetching crate from: {crate_url} and extracting Cargo.toml from it");

    let manifest_dir_path: PathBuf = format!("{name}-{version}").into();

    let mut manifest_visitor = ManifestVisitor::new(manifest_dir_path);

    Download::new(client, Url::parse(&crate_url)?)
        .and_visit_tar(TarBasedFmt::Tgz, &mut manifest_visitor)
        .await?;

    manifest_visitor.load_manifest()
}
