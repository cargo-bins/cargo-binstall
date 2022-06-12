use std::fs::File;
use std::io::{BufRead, Read};
use std::path::Path;

use flate2::bufread::GzDecoder;
use log::debug;
use tar::{Archive, Entries};
use xz2::bufread::XzDecoder;
use zip::read::ZipArchive;
use zstd::stream::Decoder as ZstdDecoder;

use crate::{BinstallError, TarBasedFmt};

/// Visitor must iterate over all entries.
/// Entires can be in arbitary order.
pub trait TarEntriesVisitor {
    fn visit<R: Read>(&mut self, entries: Entries<'_, R>) -> Result<(), BinstallError>;
}

impl<V: TarEntriesVisitor> TarEntriesVisitor for &mut V {
    fn visit<R: Read>(&mut self, entries: Entries<'_, R>) -> Result<(), BinstallError> {
        (*self).visit(entries)
    }
}

#[derive(Debug)]
pub(super) enum Op<'a, V: TarEntriesVisitor> {
    UnpackToPath(&'a Path),
    Visit(V),
}

///  * `f` - If Some, then this function will pass
///    the entries of the `dat` to it and let it decides
///    what to do with the tar.
fn untar<R: Read, V: TarEntriesVisitor>(dat: R, op: Op<'_, V>) -> Result<(), BinstallError> {
    let mut tar = Archive::new(dat);

    match op {
        Op::Visit(mut visitor) => {
            debug!("Untaring with callback");

            visitor.visit(tar.entries()?)?;
        }
        Op::UnpackToPath(path) => {
            debug!("Untaring entire tar");
            tar.unpack(path)?;
        }
    }

    debug!("Untaring completed");

    Ok(())
}

/// Extract files from the specified source onto the specified path.
///
///  * `fmt` - must not be `PkgFmt::Bin` or `PkgFmt::Zip`.
///  * `filter` - If Some, then it will pass the path of the file to it
///    and only extract ones which filter returns `true`.
///    Note that this is a best-effort and it only works when `fmt`
///    is not `PkgFmt::Bin` or `PkgFmt::Zip`.
pub(super) fn extract_compressed_from_readable<V: TarEntriesVisitor, R: BufRead>(
    dat: R,
    fmt: TarBasedFmt,
    op: Op<'_, V>,
) -> Result<(), BinstallError> {
    use TarBasedFmt::*;

    let msg = if let Op::UnpackToPath(path) = op {
        format!("destination: {path:#?}")
    } else {
        "process in-memory".to_string()
    };

    match fmt {
        Tar => {
            // Extract to install dir
            debug!("Extracting from tar archive: {msg}");

            untar(dat, op)?
        }
        Tgz => {
            // Extract to install dir
            debug!("Decompressing from tgz archive {msg}");

            let tar = GzDecoder::new(dat);
            untar(tar, op)?;
        }
        Txz => {
            // Extract to install dir
            debug!("Decompressing from txz archive {msg}");

            let tar = XzDecoder::new(dat);
            untar(tar, op)?;
        }
        Tzstd => {
            // Extract to install dir
            debug!("Decompressing from tzstd archive {msg}");

            // The error can only come from raw::Decoder::with_dictionary
            // as of zstd 0.10.2 and 0.11.2, which is specified
            // as &[] by ZstdDecoder::new, thus ZstdDecoder::new
            // should not return any error.
            let tar = ZstdDecoder::with_buffer(dat)?;
            untar(tar, op)?;
        }
    };

    Ok(())
}

pub(super) fn unzip(dat: File, dst: &Path) -> Result<(), BinstallError> {
    debug!("Decompressing from zip archive to `{dst:?}`");

    let mut zip = ZipArchive::new(dat)?;
    zip.extract(dst)?;

    Ok(())
}
