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

pub(super) fn create_tar_decoder(
    dat: impl BufRead + 'static,
    fmt: TarBasedFmt,
) -> Result<Archive<Box<dyn Read>>, BinstallError> {
    use TarBasedFmt::*;

    let r: Box<dyn Read> = match fmt {
        Tar => Box::new(dat),
        Tgz => Box::new(GzDecoder::new(dat)),
        Txz => Box::new(XzDecoder::new(dat)),
        Tzstd => {
            // The error can only come from raw::Decoder::with_dictionary
            // as of zstd 0.10.2 and 0.11.2, which is specified
            // as &[] by ZstdDecoder::new, thus ZstdDecoder::new
            // should not return any error.
            Box::new(ZstdDecoder::with_buffer(dat)?)
        }
    };

    Ok(Archive::new(r))
}

pub(super) fn unzip(dat: File, dst: &Path) -> Result<(), BinstallError> {
    debug!("Decompressing from zip archive to `{dst:?}`");

    let mut zip = ZipArchive::new(dat)?;
    zip.extract(dst)?;

    Ok(())
}
