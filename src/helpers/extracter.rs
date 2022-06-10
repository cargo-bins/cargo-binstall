use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use flate2::read::GzDecoder;
use log::debug;
use tar::Archive;
use xz2::read::XzDecoder;
use zip::read::ZipArchive;
use zstd::stream::Decoder as ZstdDecoder;

use crate::{BinstallError, PkgFmt};

///  * `filter` - If Some, then it will pass the path of the file to it
///    and only extract ones which filter returns `true`.
///    Note that this is a best-effort and it only works when `fmt`
///    is not `PkgFmt::Bin` or `PkgFmt::Zip`.
fn untar<Filter: FnMut(&Path) -> bool>(
    dat: impl Read,
    path: &Path,
    filter: Option<Filter>,
) -> Result<(), BinstallError> {
    let mut tar = Archive::new(dat);

    if let Some(mut filter) = filter {
        debug!("Untaring with filter");

        for res in tar.entries()? {
            let mut entry = res?;
            let entry_path = entry.path()?;

            if filter(&entry_path) {
                debug!("Extracting {entry_path:#?}");

                let dst = path.join(entry_path);

                fs::create_dir_all(dst.parent().unwrap())?;

                entry.unpack(dst)?;
            }
        }
    } else {
        debug!("Untaring entire tar");
        tar.unpack(path)?;
    }

    Ok(())
}

/// Extract files from the specified source onto the specified path.
///
///  * `fmt` - must not be `PkgFmt::Bin` or `PkgFmt::Zip`.
///  * `filter` - If Some, then it will pass the path of the file to it
///    and only extract ones which filter returns `true`.
///    Note that this is a best-effort and it only works when `fmt`
///    is not `PkgFmt::Bin` or `PkgFmt::Zip`.
pub(crate) fn extract_compressed_from_readable<Filter: FnMut(&Path) -> bool>(
    dat: impl Read,
    fmt: PkgFmt,
    path: &Path,
    filter: Option<Filter>,
) -> Result<(), BinstallError> {
    match fmt {
        PkgFmt::Tar => {
            // Extract to install dir
            debug!("Extracting from tar archive to `{path:?}`");

            untar(dat, path, filter)?
        }
        PkgFmt::Tgz => {
            // Extract to install dir
            debug!("Decompressing from tgz archive to `{path:?}`");

            let tar = GzDecoder::new(dat);
            untar(tar, path, filter)?;
        }
        PkgFmt::Txz => {
            // Extract to install dir
            debug!("Decompressing from txz archive to `{path:?}`");

            let tar = XzDecoder::new(dat);
            untar(tar, path, filter)?;
        }
        PkgFmt::Tzstd => {
            // Extract to install dir
            debug!("Decompressing from tzstd archive to `{path:?}`");

            // The error can only come from raw::Decoder::with_dictionary
            // as of zstd 0.10.2 and 0.11.2, which is specified
            // as &[] by ZstdDecoder::new, thus ZstdDecoder::new
            // should not return any error.
            let tar = ZstdDecoder::new(dat)?;
            untar(tar, path, filter)?;
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
