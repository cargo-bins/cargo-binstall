use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumIter, EnumString};

/// Binary format enumeration
#[derive(
    Debug, Display, Copy, Clone, Eq, PartialEq, Serialize, Deserialize, EnumString, EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(ascii_case_insensitive)]
pub enum PkgFmt {
    /// Download format is TAR (uncompressed)
    Tar,
    /// Download format is TAR + Bzip2
    Tbz2,
    /// Download format is TGZ (TAR + GZip)
    Tgz,
    /// Download format is TAR + XZ
    Txz,
    /// Download format is TAR + Zstd
    Tzstd,
    /// Download format is Zip
    Zip,
    /// Download format is raw / binary
    Bin,
}

impl Default for PkgFmt {
    fn default() -> Self {
        Self::Tgz
    }
}

impl PkgFmt {
    /// If self is one of the tar based formats, return Some.
    pub fn decompose(self) -> PkgFmtDecomposed {
        match self {
            PkgFmt::Tar => PkgFmtDecomposed::Tar(TarBasedFmt::Tar),
            PkgFmt::Tbz2 => PkgFmtDecomposed::Tar(TarBasedFmt::Tbz2),
            PkgFmt::Tgz => PkgFmtDecomposed::Tar(TarBasedFmt::Tgz),
            PkgFmt::Txz => PkgFmtDecomposed::Tar(TarBasedFmt::Txz),
            PkgFmt::Tzstd => PkgFmtDecomposed::Tar(TarBasedFmt::Tzstd),
            PkgFmt::Bin => PkgFmtDecomposed::Bin,
            PkgFmt::Zip => PkgFmtDecomposed::Zip,
        }
    }

    /// List of possible file extensions for the format
    /// (with prefix `.`).
    ///
    /// * `is_windows` - if true and `self == PkgFmt::Bin`, then it will return
    ///   `.exe` in additional to other bin extension names.
    pub fn extensions(self, is_windows: bool) -> &'static [&'static str] {
        match self {
            PkgFmt::Tar => &[".tar"],
            PkgFmt::Tbz2 => &[".tbz2", ".tar.bz2", ".tbz", ".tar.bz"],
            PkgFmt::Tgz => &[".tgz", ".tar.gz"],
            PkgFmt::Txz => &[".txz", ".tar.xz"],
            PkgFmt::Tzstd => &[".tzstd", ".tzst", ".tar.zst"],
            PkgFmt::Bin => {
                if is_windows {
                    &[".bin", "", ".exe"]
                } else {
                    &[".bin", ""]
                }
            }
            PkgFmt::Zip => &[".zip"],
        }
    }

    /// Given the pkg-url template, guess the possible pkg-fmt.
    pub fn guess_pkg_format(pkg_url: &str) -> Option<Self> {
        let mut it = pkg_url.rsplitn(3, '.');

        let guess = match it.next()? {
            "tar" => Some(PkgFmt::Tar),

            "tbz2" | "tbz" => Some(PkgFmt::Tbz2),
            "bz2" | "bz" if it.next() == Some("tar") => Some(PkgFmt::Tbz2),

            "tgz" => Some(PkgFmt::Tgz),
            "gz" if it.next() == Some("tar") => Some(PkgFmt::Tgz),

            "txz" => Some(PkgFmt::Txz),
            "xz" if it.next() == Some("tar") => Some(PkgFmt::Txz),

            "tzstd" | "tzst" => Some(PkgFmt::Tzstd),
            "zst" if it.next() == Some("tar") => Some(PkgFmt::Tzstd),

            "exe" | "bin" => Some(PkgFmt::Bin),
            "zip" => Some(PkgFmt::Zip),

            _ => None,
        };

        if it.next().is_some() {
            guess
        } else {
            None
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PkgFmtDecomposed {
    Tar(TarBasedFmt),
    Bin,
    Zip,
}

#[derive(Debug, Display, Copy, Clone, Eq, PartialEq)]
pub enum TarBasedFmt {
    /// Download format is TAR (uncompressed)
    Tar,
    /// Download format is TAR + Bzip2
    Tbz2,
    /// Download format is TGZ (TAR + GZip)
    Tgz,
    /// Download format is TAR + XZ
    Txz,
    /// Download format is TAR + Zstd
    Tzstd,
}

impl From<TarBasedFmt> for PkgFmt {
    fn from(fmt: TarBasedFmt) -> Self {
        match fmt {
            TarBasedFmt::Tar => PkgFmt::Tar,
            TarBasedFmt::Tbz2 => PkgFmt::Tbz2,
            TarBasedFmt::Tgz => PkgFmt::Tgz,
            TarBasedFmt::Txz => PkgFmt::Txz,
            TarBasedFmt::Tzstd => PkgFmt::Tzstd,
        }
    }
}
