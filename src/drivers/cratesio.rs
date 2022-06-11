use std::path::{Path, PathBuf};
use std::time::Duration;

use crates_io_api::AsyncClient;
use log::debug;
use url::Url;

use super::find_version;
use crate::{helpers::*, BinstallError, TarBasedFmt};

/// Fetch a crate Cargo.toml by name and version from crates.io
pub async fn fetch_crate_cratesio(
    name: &str,
    version_req: &str,
    temp_dir: &Path,
) -> Result<PathBuf, BinstallError> {
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

    let crate_dir: PathBuf = format!("{name}-{version_name}").into();
    let crate_path = temp_dir.join(&crate_dir);

    let cargo_toml = crate_dir.join("Cargo.toml");
    let src = crate_dir.join("src");
    let main = src.join("main.rs");
    let bin = src.join("bin");

    download_and_extract_with_filter(
        Url::parse(&crate_url)?,
        TarBasedFmt::Tgz,
        &temp_dir,
        move |path: &Path| path == cargo_toml || path == main || path.starts_with(&bin),
    )
    .await?;

    // Return crate directory
    Ok(crate_path)
}
