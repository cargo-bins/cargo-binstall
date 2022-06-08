use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crates_io_api::AsyncClient;
use log::debug;
use semver::{Version, VersionReq};
use url::Url;

use crate::{helpers::*, BinstallError, PkgFmt};

fn find_version<'a, V: Iterator<Item = &'a String>>(
    requirement: &str,
    version_iter: V,
) -> Result<Version, BinstallError> {
    // Parse version requirement
    let version_req = VersionReq::parse(requirement).map_err(|err| BinstallError::VersionReq {
        req: requirement.into(),
        err,
    })?;

    // Filter for matching versions
    let filtered: BTreeSet<_> = version_iter
        .filter_map(|v| {
            // Remove leading `v` for git tags
            let ver_str = match v.strip_prefix('s') {
                Some(v) => v,
                None => v,
            };

            // Parse out version
            let ver = Version::parse(ver_str).ok()?;
            debug!("Version: {:?}", ver);

            // Filter by version match
            if version_req.matches(&ver) {
                Some(ver)
            } else {
                None
            }
        })
        .collect();

    debug!("Filtered: {:?}", filtered);

    // Return highest version
    filtered
        .iter()
        .max()
        .cloned()
        .ok_or(BinstallError::VersionMismatch { req: version_req })
}

/// Fetch a crate by name and version from crates.io
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
    let tgz_path = temp_dir.join(format!("{name}.tgz"));

    debug!("Fetching crate from: {crate_url}");

    // Download crate
    download(Url::parse(&crate_url)?, &tgz_path).await?;

    // Decompress downloaded tgz
    debug!("Decompressing crate archive");
    extract(&tgz_path, PkgFmt::Tgz, &temp_dir)?;
    let crate_path = temp_dir.join(format!("{name}-{version_name}"));

    // Return crate directory
    Ok(crate_path)
}

/// Fetch a crate by name and version from github
/// TODO: implement this
pub async fn fetch_crate_gh_releases(
    _name: &str,
    _version: Option<&str>,
    _temp_dir: &Path,
) -> Result<PathBuf, BinstallError> {
    unimplemented!();
}
