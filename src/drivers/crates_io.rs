use std::path::PathBuf;
use std::time::Duration;

use cargo_toml::Manifest;
use crates_io_api::AsyncClient;
use log::debug;
use url::Url;

use super::{find_version, ManifestVisitor};
use crate::{helpers::*, BinstallError, Meta, TarBasedFmt};

/// Fetch a crate Cargo.toml by name and version from crates.io
pub async fn fetch_crate_cratesio(
    name: &str,
    version_req: &str,
) -> Result<Manifest<Meta>, BinstallError> {
    // Fetch / update index
    debug!("Looking up crate information");

    // Build crates.io api client
    let api_client = AsyncClient::new(
        "cargo-binstall (https://github.com/ryankurte/cargo-binstall)",
        Duration::from_millis(100),
    )
    .expect("bug: invalid user agent");

    // Fetch online crate information
    let base_info =
        api_client
            .get_crate(name.as_ref())
            .await
            .map_err(|err| BinstallError::CratesIoApi {
                crate_name: name.into(),
                err,
            })?;

    // Locate matching version
    let version_iter =
        base_info
            .versions
            .iter()
            .filter_map(|v| if !v.yanked { Some(&v.num) } else { None });
    let version_name = find_version(version_req, version_iter)?;

    // Fetch information for the filtered version
    let version = base_info
        .versions
        .iter()
        .find(|v| v.num == version_name.to_string())
        .ok_or_else(|| BinstallError::VersionUnavailable {
            crate_name: name.into(),
            v: version_name.clone(),
        })?;

    debug!("Found information for crate version: '{}'", version.num);

    // Download crate to temporary dir (crates.io or git?)
    let crate_url = format!("https://crates.io/{}", version.dl_path);

    debug!("Fetching crate from: {crate_url} and extracting Cargo.toml from it");

    let manifest_dir_path: PathBuf = format!("{name}-{version_name}").into();

    download_tar_based_and_visit(
        Url::parse(&crate_url)?,
        TarBasedFmt::Tgz,
        ManifestVisitor::new(manifest_dir_path),
    )
    .await?
    .load_manifest()
}
