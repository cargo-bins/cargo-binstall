use std::io::{self, BufRead, Read};

use bzip2::bufread::BzDecoder;
use flate2::bufread::GzDecoder;
use liblzma::bufread::XzDecoder;
use tar::Archive;
use zstd::stream::Decoder as ZstdDecoder;

use super::TarBasedFmt;

pub fn create_tar_decoder(
    dat: impl BufRead + 'static,
    fmt: TarBasedFmt,
) -> io::Result<Archive<Box<dyn Read>>> {
    use TarBasedFmt::*;

    let r: Box<dyn Read> = match fmt {
        Tar => Box::new(dat),
        Tbz2 => Box::new(BzDecoder::new(dat)),
        Tgz => Box::new(GzDecoder::new(dat)),
        Txz => Box::new(XzDecoder::new(dat)),
        Tzstd => {
            // The error can only come from raw::Decoder::with_dictionary as of zstd 0.10.2 and
            // 0.11.2, which is specified as `&[]` by `ZstdDecoder::new`, thus `ZstdDecoder::new`
            // should not return any error.
            Box::new(ZstdDecoder::with_buffer(dat)?)
        }
    };

    Ok(Archive::new(r))
}
