use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumString};

/// Binary format enumeration
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize, EnumString)]
#[serde(rename_all = "snake_case")]
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

    /// List of possible file extensions for the format.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            PkgFmt::Tar => &["tar"],
            PkgFmt::Tbz2 => &["tbz2", "tar.bz2"],
            PkgFmt::Tgz => &["tgz", "tar.gz"],
            PkgFmt::Txz => &["txz", "tar.xz"],
            PkgFmt::Tzstd => &["tzstd", "tzst", "tar.zst"],
            PkgFmt::Bin => &["bin", "exe"],
            PkgFmt::Zip => &["zip"],
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
