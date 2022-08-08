use std::path::PathBuf;

use cargo_toml::Manifest;
use crates_io_api::AsyncClient;
use log::debug;
use reqwest::Client;
use semver::VersionReq;
use url::Url;

use super::find_version;
use crate::{helpers::*, BinstallError, Meta, TarBasedFmt};

mod vfs;

mod visitor;
use visitor::ManifestVisitor;

/// Fetch a crate Cargo.toml by name and version from crates.io
pub async fn fetch_crate_cratesio(
    client: &Client,
    crates_io_api_client: &AsyncClient,
    name: &str,
    version_req: &VersionReq,
) -> Result<Manifest<Meta>, BinstallError> {
    // Fetch / update index
    debug!("Looking up crate information");

    // Fetch online crate information
    let base_info = crates_io_api_client
        .get_crate(name.as_ref())
        .await
        .map_err(|err| BinstallError::CratesIoApi {
            crate_name: name.into(),
            err,
        })?;

    // Locate matching version
    let version_iter = base_info.versions.iter().filter(|v| !v.yanked);
    let (version, version_name) = find_version(version_req, version_iter)?;

    debug!("Found information for crate version: '{}'", version.num);

    // Download crate to temporary dir (crates.io or git?)
    let crate_url = format!("https://crates.io/{}", version.dl_path);

    debug!("Fetching crate from: {crate_url} and extracting Cargo.toml from it");

    let manifest_dir_path: PathBuf = format!("{name}-{version_name}").into();

    download_tar_based_and_visit(
        client,
        Url::parse(&crate_url)?,
        TarBasedFmt::Tgz,
        ManifestVisitor::new(manifest_dir_path),
    )
    .await
}
