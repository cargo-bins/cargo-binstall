
use std::time::Duration;
use std::path::{Path, PathBuf};

use log::{debug, error};

use crates_io_api::AsyncClient;

use crate::PkgFmt;
use crate::helpers::*;

/// Fetch a crate by name and version from crates.io
pub async fn fetch_crate_cratesio(name: &str, version: Option<&str>, temp_dir: &Path) -> Result<PathBuf, anyhow::Error> {
    // Build crates.io api client and fetch info
    let api_client = AsyncClient::new("cargo-binstall (https://github.com/ryankurte/cargo-binstall)", Duration::from_millis(100))?;

    debug!("Fetching information for crate: '{}'", name);

    // Fetch overall crate info
    let info = match api_client.get_crate(name.as_ref()).await {
        Ok(i) => i,
        Err(e) => {
            error!("Error fetching information for crate {}: {}", name, e);
            return Err(e.into())
        }
    };

    // Use specified or latest version
    let version_num = match version {
        Some(v) => v.to_string(),
        None => info.crate_data.max_version,
    };

    // Fetch crates.io information for the specified version
    // Note it is not viable to use a semver match here as crates.io
    // appears to elide alpha and yanked versions in the generic response...
    let versions = info.versions.clone();
    let version = match versions.iter().find(|v| v.num == version_num) {
        Some(v) => v,
        None => {
            error!("No crates.io information found for crate: '{}' version: '{}'", 
                    name, version_num);
            return Err(anyhow::anyhow!("No crate information found"));
        }
    };

    debug!("Found information for crate version: '{}'", version.num);
    
    // Download crate to temporary dir (crates.io or git?)
    let crate_url = format!("https://crates.io/{}", version.dl_path);
    let tgz_path = temp_dir.join(format!("{}.tgz", name));

    debug!("Fetching crate from: {}", crate_url);    

    // Download crate
    download(&crate_url, &tgz_path).await?;

    // Decompress downloaded tgz
    debug!("Decompressing crate archive");
    extract(&tgz_path, PkgFmt::Tgz, &temp_dir)?;
    let crate_path = temp_dir.join(format!("{}-{}", name, version_num));

    // Return crate directory
    Ok(crate_path)
}

/// Fetch a crate by name and version from github
/// TODO: implement this
pub async fn fetch_crate_gh_releases(_name: &str, _version: Option<&str>, _temp_dir: &Path) -> Result<PathBuf, anyhow::Error> {

    unimplemented!();
}

