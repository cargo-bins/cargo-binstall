use std::path::PathBuf;

use cargo_toml::Manifest;
use crates_io_api::AsyncClient;
use semver::VersionReq;
use tracing::debug;

use crate::{
    errors::BinstallError,
    helpers::{
        download::Download,
        remote::{Client, Url},
        signal::wait_on_cancellation_signal,
    },
    manifests::cargo_toml_binstall::{Meta, TarBasedFmt},
};

use super::find_version;

mod vfs;

mod visitor;
use visitor::ManifestVisitor;

/// Fetch a crate Cargo.toml by name and version from crates.io
pub async fn fetch_crate_cratesio(
    client: Client,
    crates_io_api_client: &AsyncClient,
    name: &str,
    version_req: &VersionReq,
) -> Result<Manifest<Meta>, BinstallError> {
    // Fetch / update index
    debug!("Looking up crate information");

    // Fetch online crate information
    let base_info =
        crates_io_api_client
            .get_crate(name)
            .await
            .map_err(|err| BinstallError::CratesIoApi {
                crate_name: name.into(),
                err: Box::new(err),
            })?;

    // Locate matching version
    let version_iter = base_info.versions.iter().filter(|v| !v.yanked);
    let (version, version_name) = find_version(version_req, version_iter)?;

    debug!("Found information for crate version: '{}'", version.num);

    // Download crate to temporary dir (crates.io or git?)
    let crate_url = format!("https://crates.io/{}", version.dl_path);

    debug!("Fetching crate from: {crate_url} and extracting Cargo.toml from it");

    let manifest_dir_path: PathBuf = format!("{name}-{version_name}").into();

    Ok(Download::new(client, Url::parse(&crate_url)?)
        .and_visit_tar(
            TarBasedFmt::Tgz,
            ManifestVisitor::new(manifest_dir_path),
            Some(Box::pin(wait_on_cancellation_signal())),
        )
        .await?)
}
