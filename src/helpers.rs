
use std::path::Path;

use log::{debug, error};

use flate2::read::GzDecoder;
use tar::Archive;


use super::PkgFmt;


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
            debug!("Extracting from archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());

            let dat = std::fs::File::open(source)?;
            let mut tar = Archive::new(dat);

            tar.unpack(path)?;
        },
        PkgFmt::Tgz => {
            // Extract to install dir
            debug!("Decompressing from archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());

            let dat = std::fs::File::open(source)?;
            let tar = GzDecoder::new(dat);
            let mut tgz = Archive::new(tar);

            tgz.unpack(path)?;
        },
        PkgFmt::Bin => {
            debug!("Copying data from archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());
            // Copy to install dir
            std::fs::copy(source, path)?;
        },
    };

    Ok(())
}

