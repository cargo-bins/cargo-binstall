use std::path::PathBuf;

use cargo_toml::Manifest;
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

#[derive(Deserialize)]
struct Response {
    #[serde(rename = "crate")]
    inner: Crate,
}

#[derive(Deserialize)]
struct Crate {
    max_stable_version: String,
}

mod vfs;

mod visitor;
use visitor::ManifestVisitor;

/// Find the crate by name, get its latest stable version, retrieve its
/// Cargo.toml and infer all its bins.
pub async fn fetch_crate_cratesio(
    client: Client,
    name: &str,
    version_req: &VersionReq,
) -> Result<Manifest<Meta>, BinstallError> {
    // Fetch / update index
    debug!("Looking up crate information");

    let response: Response = client
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
        })?
        .json()
        .await?;

    let version = response.inner.max_stable_version;

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
