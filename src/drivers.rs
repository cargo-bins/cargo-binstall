
use std::time::Duration;
use std::path::{Path, PathBuf};

use log::{debug};
use anyhow::{Context, anyhow};
use semver::{Version, VersionReq};

use crates_io_api::AsyncClient;

use crate::PkgFmt;
use crate::helpers::*;

fn find_version<'a, V: Iterator<Item=&'a str>>(requirement: &str, version_iter: V) -> Result<String, anyhow::Error> {
    // Parse version requirement
    let version_req = VersionReq::parse(requirement)?;

    // Filter for matching versions
    let mut filtered: Vec<_> = version_iter.filter(|v| {
        // Remove leading `v` for git tags
        let ver_str = match v.strip_prefix("s") {
            Some(v) => v,
            None => v,
        };

        // Parse out version
        let ver = match Version::parse(ver_str) {
            Ok(sv) => sv,
            Err(_) => return false,
        };

        debug!("Version: {:?}", ver);

        // Filter by version match
        version_req.matches(&ver)
    }).collect();

    // Sort by highest matching version
    filtered.sort_by(|a, b| {
        let a = Version::parse(a).unwrap();
        let b = Version::parse(b).unwrap();

        b.partial_cmp(&a).unwrap()
    });

    debug!("Filtered: {:?}", filtered);

    // Return highest version
    match filtered.get(0) {
        Some(v) => Ok(v.to_string()),
        None => Err(anyhow!("No matching version for requirement: '{}'", version_req))
    }
}

/// Fetch a crate by name and version from crates.io
pub async fn fetch_crate_cratesio(name: &str, version_req: &str, temp_dir: &Path) -> Result<PathBuf, anyhow::Error> {

    // Fetch / update index
    debug!("Updating crates.io index");
    let mut index = crates_index::Index::new_cargo_default()?;
    index.update()?;

    // Lookup crate in index
    debug!("Looking up crate information");
    let base_info = match index.crate_(name) {
        Some(i) => i,
        None => {
            return Err(anyhow::anyhow!("Error fetching information for crate {}", name));
        }
    };

    // Locate matching version
    let version_iter = base_info.versions().iter().map(|v| v.version() );
    let version_name = find_version(version_req, version_iter)?;
    
    // Build crates.io api client
    let api_client = AsyncClient::new("cargo-binstall (https://github.com/ryankurte/cargo-binstall)", Duration::from_millis(100))?;

    // Fetch online crate information
    let crate_info = api_client.get_crate(name.as_ref()).await
        .context("Error fetching crate information")?;

    // Fetch information for the filtered version
    let version = match crate_info.versions.iter().find(|v| v.num == version_name) {
        Some(v) => v,
        None => {
            return Err(anyhow::anyhow!("No information found for crate: '{}' version: '{}'", 
                    name, version_name));
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
    let crate_path = temp_dir.join(format!("{}-{}", name, version_name));

    // Return crate directory
    Ok(crate_path)
}

/// Fetch a crate by name and version from github
/// TODO: implement this
pub async fn fetch_crate_gh_releases(_name: &str, _version: Option<&str>, _temp_dir: &Path) -> Result<PathBuf, anyhow::Error> {

    unimplemented!();
}

