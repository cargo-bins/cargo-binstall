use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use log::{debug, error, info};

use cargo_toml::Manifest;
use flate2::read::GzDecoder;
use serde::Serialize;
use std::io::Read;
use tar::Archive;
use tinytemplate::TinyTemplate;
use xz2::read::XzDecoder;
use zip::read::ZipArchive;

use crate::Meta;

use super::PkgFmt;

/// Load binstall metadata from the crate `Cargo.toml` at the provided path
pub fn load_manifest_path(mut manifest_path: PathBuf) -> Result<Manifest<Meta>, anyhow::Error> {
    debug!("Reading manifest: {}", manifest_path.display());

    if manifest_path.is_dir() {
        manifest_path = manifest_path.join("Cargo.toml");
    }

    if !manifest_path.exists() {
        error!(
            "Manifest at '{:?}' could not be found",
            manifest_path.display()
        );
        return Err(anyhow::anyhow!("Manifest could not be found"));
    }

    // Load and parse manifest (this checks file system for binary output names)
    let manifest = Manifest::<Meta>::from_path_with_metadata(manifest_path)?;

    // Return metadata
    Ok(manifest)
}

pub async fn remote_exists(url: &str, method: reqwest::Method) -> Result<bool, anyhow::Error> {
    let req = reqwest::Client::new().request(method, url).send().await?;
    Ok(req.status().is_success())
}

/// Download a file from the provided URL to the provided path
pub async fn download(url: &str) -> Result<bytes::Bytes, anyhow::Error> {
    debug!("Downloading from: '{}'", url);

    let resp = reqwest::get(url).await?;

    if !resp.status().is_success() {
        error!("Download error: {}", resp.status());
        return Err(anyhow::anyhow!(resp.status()));
    }

    Ok(resp.bytes().await?)
}

// Extracts a file at path from tar archive to byte vector
fn extract_file_from_tar_archive<T: std::io::Read>(
    tar_archive: &mut Archive<T>,
    path: PathBuf,
) -> Result<Vec<u8>, anyhow::Error> {
    let mut file = tar_archive
        .entries()?
        .find(|e| {
            if let Ok(value) = e.as_ref() {
                if let Ok(file_path) = value.path() {
                    file_path == path
                } else {
                    false
                }
            } else {
                false
            }
        })
        .unwrap()?;

    let mut buffer = Vec::with_capacity(file.size() as usize);

    file.read_to_end(&mut buffer)?;

    Ok(buffer)
}

/// Extract files from the specified source onto the specified path
pub fn extract_file(
    source: &bytes::Bytes,
    fmt: PkgFmt,
    file: PathBuf,
) -> Result<Vec<u8>, anyhow::Error> {
    match fmt {
        PkgFmt::Tar => {
            // Extract to install dir
            debug!("Extracting from tar archive '{:?}'", source.as_ref(),);

            let mut tar = Archive::new(&source[..]);

            extract_file_from_tar_archive(&mut tar, file)
        }
        PkgFmt::Tgz => {
            // Extract to install dir
            debug!("Decompressing from tgz archive '{:?}'", source.as_ref());

            let tar = GzDecoder::new(&source[..]);
            let mut tgz = Archive::new(tar);

            extract_file_from_tar_archive(&mut tgz, file)
        }
        PkgFmt::Txz => {
            // Extract to install dir
            debug!("Decompressing from txz archive '{:?}'", source.as_ref());

            let tar = XzDecoder::new(&source[..]);
            let mut txz = Archive::new(tar);

            extract_file_from_tar_archive(&mut txz, file)
        }
        PkgFmt::Zip => {
            // Extract to install dir
            debug!("Decompressing from zip archive '{:?}'", source.as_ref());

            let mut zip = ZipArchive::new(Cursor::new(&source[..]))?;

            let mut file = zip.by_name(file.to_str().unwrap())?;

            let mut buffer = Vec::with_capacity(file.size() as usize);

            file.read_to_end(&mut buffer)?;

            Ok(buffer)
        }
        PkgFmt::Bin => {
            debug!("Copying binary '{:?}'", source.as_ref());
            // Copy to install dir
            Ok(source.to_vec())
        }
    }
}

/// Fetch install path from environment
/// roughly follows https://doc.rust-lang.org/cargo/commands/cargo-install.html#description
pub fn get_install_path<P: AsRef<Path>>(install_path: Option<P>) -> Option<PathBuf> {
    // Command line override first first
    if let Some(p) = install_path {
        return Some(PathBuf::from(p.as_ref()));
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
        _ => Err(anyhow::anyhow!(
            "Valid options are 'yes', 'no', please try again"
        )),
    }
}

pub trait Template: Serialize {
    fn render(&self, template: &str) -> Result<String, anyhow::Error>
    where
        Self: Sized,
    {
        // Create template instance
        let mut tt = TinyTemplate::new();

        // Add template to instance
        tt.add_template("path", template)?;

        // Render output
        Ok(tt.render("path", self)?)
    }
}
