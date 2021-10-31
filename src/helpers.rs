
use std::path::{Path, PathBuf};

use log::{debug, info, error};

use cargo_toml::{Manifest};
use flate2::read::GzDecoder;
use tar::Archive;
use xz2::read::XzDecoder;
use zip::read::ZipArchive;

use crate::{Meta};

use super::PkgFmt;

/// Load binstall metadata from the crate `Cargo.toml` at the provided path
pub fn load_manifest_path<P: AsRef<Path>>(manifest_path: P) -> Result<Manifest<Meta>, anyhow::Error> {
    debug!("Reading manifest: {}", manifest_path.as_ref().display());

    // Load and parse manifest (this checks file system for binary output names)
    let manifest = Manifest::<Meta>::from_path_with_metadata(manifest_path)?;

    // Return metadata
    Ok(manifest)
}

/// Download a file from the provided URL to the provided path
pub async fn download<P: AsRef<Path>>(url: &str, path: P) -> Result<(), anyhow::Error> {

    debug!("Downloading from: '{}'", url);

    let resp = reqwest::get(url).await?;

    if !resp.status().is_success() {
        error!("Download error: {}", resp.status());
        return Err(anyhow::anyhow!(resp.status()));
    }

    let bytes = resp.bytes().await?;

    debug!("Download OK, writing to file: '{:?}'", path.as_ref());

    std::fs::write(&path, bytes)?;

    Ok(())
}

/// Extract files from the specified source onto the specified path
pub fn extract<S: AsRef<Path>, P: AsRef<Path>>(source: S, fmt: PkgFmt, path: P) -> Result<(), anyhow::Error> {
    match fmt {
        PkgFmt::Tar => {
            // Extract to install dir
            debug!("Extracting from tar archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());

            let dat = std::fs::File::open(source)?;
            let mut tar = Archive::new(dat);

            tar.unpack(path)?;
        },
        PkgFmt::Tgz | PkgFmt::TarGz => {
            // Extract to install dir
            debug!("Decompressing from tgz archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());

            let dat = std::fs::File::open(source)?;
            let tar = GzDecoder::new(dat);
            let mut tgz = Archive::new(tar);

            tgz.unpack(path)?;
        },
        PkgFmt::Txz => {
            // Extract to install dir
            debug!("Decompressing from txz archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());

            let dat = std::fs::File::open(source)?;
            let tar = XzDecoder::new(dat);
            let mut txz = Archive::new(tar);

            txz.unpack(path)?;
        },
        PkgFmt::Zip => {
            // Extract to install dir
            debug!("Decompressing from zip archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());

            let dat = std::fs::File::open(source)?;
            let mut zip = ZipArchive::new(dat)?;

            zip.extract(path)?;
        },
        PkgFmt::Bin => {
            debug!("Copying binary '{:?}' to `{:?}`", source.as_ref(), path.as_ref());
            // Copy to install dir
            std::fs::copy(source, path)?;
        },
    };

    Ok(())
}

/// Fetch install path from environment
/// roughly follows https://doc.rust-lang.org/cargo/commands/cargo-install.html#description
pub fn get_install_path<P: AsRef<Path>>(install_path: Option<P>) -> Option<PathBuf> {
    // Command line override first first
    if let Some(p) = install_path {
        return Some(PathBuf::from(p.as_ref()))
    }

    // Environmental variables
    if let Ok(p) = std::env::var("CARGO_INSTALL_ROOT") {
        debug!("using CARGO_INSTALL_ROOT ({})", p);
        let b = PathBuf::from(p);
        return Some(b.join("bin"));
    }
    if let Ok(p) = std::env::var("CARGO_HOME") {
        debug!("using CARGO_HOME ({})", p);
        let b = PathBuf::from(p);
        return Some(b.join("bin"));
    }

    // Standard $HOME/.cargo/bin
    if let Some(d) = dirs::home_dir() {
        let d = d.join(".cargo/bin");
        if d.exists() {
            debug!("using $HOME/.cargo/bin");

            return Some(d);
        }
    }

    // Local executable dir if no cargo is found
    if let Some(d) = dirs::executable_dir() {
        debug!("Fallback to {}", d.display());
        return Some(d);
    }

    None
}

pub fn confirm() -> Result<bool, anyhow::Error> {
    info!("Do you wish to continue? yes/no");

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    match input.as_str().trim() {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => {
            Err(anyhow::anyhow!("Valid options are 'yes', 'no', please try again"))
        }
    }
}
