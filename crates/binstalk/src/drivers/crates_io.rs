use std::path::PathBuf;

use cargo_toml::Manifest;
use compact_str::CompactString;
use semver::VersionReq;
use serde::Deserialize;
use tracing::debug;

use crate::{
    errors::{BinstallError, CratesIoApiError},
    helpers::{
        download::Download,
        remote::{Client, Url},
    },
    manifests::cargo_toml_binstall::{Meta, TarBasedFmt},
};

mod vfs;

mod visitor;
use visitor::ManifestVisitor;

#[derive(Deserialize)]
struct Response {
    versions: Vec<Version>,
}

#[derive(Deserialize)]
struct Version {
    num: CompactString,
    yanked: bool,
}

/// Find the crate by name, get its latest stable version matches `version_req`,
/// retrieve its Cargo.toml and infer all its bins.
pub async fn fetch_crate_cratesio(
    client: Client,
    name: &str,
    version_req: &VersionReq,
) -> Result<Manifest<Meta>, BinstallError> {
    // Fetch / update index
    debug!("Looking up crate information");

    let response: Response = client
        .get(Url::parse(&format!(
            "https://crates.io/api/v1/crates/{name}/versions"
        ))?)
        .send(true)
        .await
        .map_err(|err| {
            BinstallError::CratesIoApi(Box::new(CratesIoApiError {
                crate_name: name.into(),
                err,
            }))
        })?
        .json()
        .await?;

    let (ver_str, version) = response
        .versions
        .iter()
        .filter_map(|item| {
            if !item.yanked {
                let num = &item.num;

                // Remove leading `v` for git tags
                let ver = num.strip_prefix('v').unwrap_or(num);

                // Parse out version
                let ver = semver::Version::parse(ver).ok()?;

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
        })?;

    debug!("Found information for crate version: '{version}'");

    // Download crate to temporary dir (crates.io or git?)
    let crate_url = format!("https://crates.io/api/v1/crates/{name}/{ver_str}/download");

    debug!("Fetching crate from: {crate_url} and extracting Cargo.toml from it");

    let manifest_dir_path: PathBuf = format!("{name}-{version}").into();

    let mut manifest_visitor = ManifestVisitor::new(manifest_dir_path);

    Download::new(client, Url::parse(&crate_url)?)
        .and_visit_tar(TarBasedFmt::Tgz, &mut manifest_visitor)
        .await?;

    manifest_visitor.load_manifest()
}
