use serde::{Deserialize, Serialize};
use strum_macros::{Display, EnumIter, EnumString};

/// Binary format enumeration
#[derive(
    Debug, Display, Copy, Clone, Eq, PartialEq, Serialize, Deserialize, EnumString, EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(ascii_case_insensitive)]
#[derive(Default)]
pub enum PkgFmt {
    /// Download format is TAR (uncompressed)
    Tar,
    /// Download format is TAR + Bzip2
    Tbz2,
    /// Download format is TGZ (TAR + GZip)
    #[default]
    Tgz,
    /// Download format is TAR + XZ
    Txz,
    /// Download format is TAR + Zstd
    Tzstd,
    /// Download format is Zip
    Zip,
    /// Download format is 7z
    #[serde(rename = "7z")]
    #[strum(to_string = "7z")]
    SevenZip,
    /// Download format is raw / binary
    Bin,
    /// Download format is a single binary compressed with GZip, not a TAR
    Gz,
    /// Download format is a single binary compressed with Bzip2, not a TAR
    Bz2,
    /// Download format is a single binary compressed with XZ, not a TAR
    Xz,
    /// Download format is a single binary compressed with Zstd, not a TAR
    #[serde(alias = "zstd")]
    #[strum(to_string = "Zst", serialize = "zstd")]
    Zst,
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
            PkgFmt::SevenZip => PkgFmtDecomposed::SevenZip,
            PkgFmt::Gz => PkgFmtDecomposed::CompressedBin(CompressionFmt::Gz),
            PkgFmt::Bz2 => PkgFmtDecomposed::CompressedBin(CompressionFmt::Bz2),
            PkgFmt::Xz => PkgFmtDecomposed::CompressedBin(CompressionFmt::Xz),
            PkgFmt::Zst => PkgFmtDecomposed::CompressedBin(CompressionFmt::Zst),
        }
    }

    /// Return true if the download is the binary itself, either as-is
    /// ([`PkgFmt::Bin`]) or compressed without an archive around it
    /// ([`PkgFmt::Gz`], [`PkgFmt::Bz2`], [`PkgFmt::Xz`], [`PkgFmt::Zst`]).
    ///
    /// Such a download holds no file name, so the binary is installed under
    /// the name from the crate manifest and `bin-dir` is not used.
    pub fn is_single_file(self) -> bool {
        matches!(
            self.decompose(),
            PkgFmtDecomposed::Bin | PkgFmtDecomposed::CompressedBin(_)
        )
    }

    /// List of possible file extensions for the format
    /// (with prefix `.`).
    ///
    /// * `is_windows` - if true and `self` is [`PkgFmt::Bin`] or one of the
    ///   single-file compressed formats, then it will also return the
    ///   extension names with `.exe` in it.
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
            PkgFmt::SevenZip => &[".7z"],
            PkgFmt::Gz if is_windows => &[".gz", ".exe.gz"],
            PkgFmt::Gz => &[".gz"],
            PkgFmt::Bz2 if is_windows => &[".bz2", ".exe.bz2"],
            PkgFmt::Bz2 => &[".bz2"],
            PkgFmt::Xz if is_windows => &[".xz", ".exe.xz"],
            PkgFmt::Xz => &[".xz"],
            PkgFmt::Zst if is_windows => &[".zst", ".exe.zst"],
            PkgFmt::Zst => &[".zst"],
        }
    }

    /// Given the pkg-url template, guess the possible pkg-fmt.
    pub fn guess_pkg_format(pkg_url: &str) -> Option<Self> {
        let mut it = pkg_url.rsplitn(3, '.');

        let ext = it.next()?;
        // A bare extension with nothing before it isn't a file name.
        let before_ext = it.next()?;

        // `.tar.gz` and friends: the compression wraps a TAR, as long as there
        // is a file name in front of the `.tar`. Otherwise `.gz` and friends
        // compress the binary itself.
        let is_tar = before_ext == "tar";
        if is_tar && it.next().is_none() {
            return None;
        }

        let tar_or = |tar, single| if is_tar { tar } else { single };

        match ext {
            "tar" => Some(PkgFmt::Tar),

            "tbz2" | "tbz" => Some(PkgFmt::Tbz2),
            "bz2" | "bz" => Some(tar_or(PkgFmt::Tbz2, PkgFmt::Bz2)),

            "tgz" => Some(PkgFmt::Tgz),
            "gz" => Some(tar_or(PkgFmt::Tgz, PkgFmt::Gz)),

            "txz" => Some(PkgFmt::Txz),
            "xz" => Some(tar_or(PkgFmt::Txz, PkgFmt::Xz)),

            "tzstd" | "tzst" => Some(PkgFmt::Tzstd),
            "zst" => Some(tar_or(PkgFmt::Tzstd, PkgFmt::Zst)),

            "exe" | "bin" => Some(PkgFmt::Bin),
            "zip" => Some(PkgFmt::Zip),
            "7z" => Some(PkgFmt::SevenZip),

            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum PkgFmtDecomposed {
    Tar(TarBasedFmt),
    Bin,
    Zip,
    SevenZip,
    /// A single binary compressed without an archive around it.
    CompressedBin(CompressionFmt),
}

/// Compression of a single binary downloaded without an archive around it.
#[derive(Debug, Display, Copy, Clone, Eq, PartialEq)]
pub enum CompressionFmt {
    /// GZip
    Gz,
    /// Bzip2
    Bz2,
    /// XZ
    Xz,
    /// Zstd
    Zst,
}

impl From<CompressionFmt> for PkgFmt {
    fn from(fmt: CompressionFmt) -> Self {
        match fmt {
            CompressionFmt::Gz => PkgFmt::Gz,
            CompressionFmt::Bz2 => PkgFmt::Bz2,
            CompressionFmt::Xz => PkgFmt::Xz,
            CompressionFmt::Zst => PkgFmt::Zst,
        }
    }
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

#[cfg(test)]
mod test {
    use std::str::FromStr;

    use super::*;

    #[test]
    fn guess_pkg_format() {
        let cases = [
            ("{ name }-{ target }.tar", Some(PkgFmt::Tar)),
            ("{ name }-{ target }.tar.gz", Some(PkgFmt::Tgz)),
            ("{ name }-{ target }.tgz", Some(PkgFmt::Tgz)),
            ("{ name }-{ target }.tar.bz2", Some(PkgFmt::Tbz2)),
            ("{ name }-{ target }.tar.bz", Some(PkgFmt::Tbz2)),
            ("{ name }-{ target }.tar.xz", Some(PkgFmt::Txz)),
            ("{ name }-{ target }.tar.zst", Some(PkgFmt::Tzstd)),
            ("{ name }-{ target }.zip", Some(PkgFmt::Zip)),
            ("{ name }-{ target }.exe", Some(PkgFmt::Bin)),
            ("{ name }-{ target }.gz", Some(PkgFmt::Gz)),
            ("{ name }-{ target }.exe.gz", Some(PkgFmt::Gz)),
            ("{ name }-{ target }.bz2", Some(PkgFmt::Bz2)),
            ("{ name }-{ target }.xz", Some(PkgFmt::Xz)),
            ("{ name }-{ target }.zst", Some(PkgFmt::Zst)),
            // A dot in the version doesn't make it a TAR.
            ("{ name }-1.2.gz", Some(PkgFmt::Gz)),
            (
                "{ repo }/releases/download/{ version }/taplo-{ target-family }-{ target-arch }.gz",
                Some(PkgFmt::Gz),
            ),
            // No file name in front of the extension.
            ("gz", None),
            ("tar.gz", None),
            ("{ name }-{ target }", None),
            ("{ name }-{ target }.7z", Some(PkgFmt::SevenZip)),
            ("7z", None),
        ];

        for (pkg_url, expected) in cases {
            assert_eq!(PkgFmt::guess_pkg_format(pkg_url), expected, "{pkg_url}");
        }
    }

    #[test]
    fn single_file_formats() {
        for fmt in [PkgFmt::Gz, PkgFmt::Bz2, PkgFmt::Xz, PkgFmt::Zst] {
            assert!(fmt.is_single_file());
            assert!(matches!(
                fmt.decompose(),
                PkgFmtDecomposed::CompressedBin(compression) if PkgFmt::from(compression) == fmt
            ));

            // Each extension is guessed back as the same format.
            for is_windows in [false, true] {
                for ext in fmt.extensions(is_windows) {
                    let pkg_url = format!("{{ name }}-{{ target }}{ext}");
                    assert_eq!(PkgFmt::guess_pkg_format(&pkg_url), Some(fmt), "{pkg_url}");
                }
            }
        }

        assert!(PkgFmt::Bin.is_single_file());
        for fmt in [PkgFmt::Tar, PkgFmt::Tgz, PkgFmt::Zip, PkgFmt::SevenZip] {
            assert!(!fmt.is_single_file());
        }
    }

    #[test]
    fn parse_7z() {
        for s in ["7z", "7Z"] {
            assert_eq!(PkgFmt::from_str(s).unwrap(), PkgFmt::SevenZip, "{s}");
        }

        let toml: PkgFmt = serde_json::from_str("\"7z\"").unwrap();
        assert_eq!(toml, PkgFmt::SevenZip);
        assert_eq!(serde_json::to_string(&PkgFmt::SevenZip).unwrap(), "\"7z\"");

        // Shown to users as what they'd write in `pkg-fmt`.
        assert_eq!(PkgFmt::SevenZip.to_string(), "7z");
        assert_eq!(
            PkgFmt::guess_pkg_format("{ name }.7z"),
            Some(PkgFmt::SevenZip)
        );
    }

    #[test]
    fn parse_single_file_formats() {
        for (s, fmt) in [
            ("gz", PkgFmt::Gz),
            ("bz2", PkgFmt::Bz2),
            ("xz", PkgFmt::Xz),
            ("zst", PkgFmt::Zst),
            ("zstd", PkgFmt::Zst),
        ] {
            assert_eq!(PkgFmt::from_str(s).unwrap(), fmt, "{s}");

            let toml: PkgFmt = serde_json::from_str(&format!("\"{s}\"")).unwrap();
            assert_eq!(toml, fmt, "{s}");
        }

        assert_eq!(
            serde_json::to_string(&PkgFmt::Zst).unwrap(),
            "\"zst\"",
            "serialized as the file extension"
        );
    }
}
