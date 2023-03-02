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

mod versions;
use versions::find_max_version_matched;

#[derive(Deserialize)]
struct CrateInfo {
    #[serde(rename = "crate")]
    inner: CrateInfoInner,
}

#[derive(Deserialize)]
struct CrateInfoInner {
    max_stable_version: CompactString,
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
        find_max_version_matched(&mut response.json_deserializer().await?, version_req)?
    };

    debug!("Found information for crate version: '{version}'");

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
