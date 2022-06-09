use std::fs::File;
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use log::debug;
use tar::Archive;
use xz2::read::XzDecoder;
use zip::read::ZipArchive;
use zstd::stream::Decoder as ZstdDecoder;

use crate::{BinstallError, PkgFmt};

/// Extract files from the specified source onto the specified path.
///
///  * `fmt` - must not be `PkgFmt::Bin` or `PkgFmt::Zip`.
pub(crate) fn extract_compressed_from_readable(
    dat: impl Read,
    fmt: PkgFmt,
    path: &Path,
) -> Result<(), BinstallError> {
    match fmt {
        PkgFmt::Tar => {
            // Extract to install dir
            debug!("Extracting from tar archive to `{path:?}`");

            let mut tar = Archive::new(dat);

            tar.unpack(path)?;
        }
        PkgFmt::Tgz => {
            // Extract to install dir
            debug!("Decompressing from tgz archive to `{path:?}`");

            let tar = GzDecoder::new(dat);
            let mut tgz = Archive::new(tar);

            tgz.unpack(path)?;
        }
        PkgFmt::Txz => {
            // Extract to install dir
            debug!("Decompressing from txz archive to `{path:?}`");

            let tar = XzDecoder::new(dat);
            let mut txz = Archive::new(tar);

            txz.unpack(path)?;
        }
        PkgFmt::Tzstd => {
            // Extract to install dir
            debug!("Decompressing from tzstd archive to `{path:?}`");

            // The error can only come from raw::Decoder::with_dictionary
            // as of zstd 0.10.2 and 0.11.2, which is specified
            // as &[] by ZstdDecoder::new, thus ZstdDecoder::new
            // should not return any error.
            let tar = ZstdDecoder::new(dat)?;
            let mut txz = Archive::new(tar);

            txz.unpack(path)?;
        }
        PkgFmt::Zip => panic!("Unexpected PkgFmt::Zip!"),
        PkgFmt::Bin => panic!("Unexpected PkgFmt::Bin!"),
    };

    Ok(())
}

pub(crate) fn unzip(dat: File, dst: &Path) -> Result<(), BinstallError> {
    debug!("Decompressing from zip archive to `{dst:?}`");

    let mut zip = ZipArchive::new(dat)?;
    zip.extract(dst)?;

    Ok(())
}
