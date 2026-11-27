//! Check that a downloaded executable can run on this system, before it
//! is installed.
//!
//! Detecting the host's targets says which *kinds* of binaries run here,
//! but a given binary can still need a dynamic loader the host lacks (a
//! musl binary linked dynamically on a glibc host), a shared library that
//! isn't installed, or a newer glibc than the host's.
//!
//! [`check`] asks the host's own dynamic loader: it runs it in list mode
//! (`ld.so --list`, what `ldd` does) on the binary, which resolves its
//! libraries and symbol versions without running any of its code. Only
//! loaders at the standard paths of the probe table are run, and only if
//! they are real loaders, never a `PT_INTERP` the binary makes up. When
//! the answer isn't clear, the binary passes. The check only does
//! something on Linux and Android.

use std::{error::Error, fmt};

cfg_select! {
    any(target_os = "linux", target_os = "android") => {
        mod linux;
        pub use linux::check;
    }
    _ => {
        /// Check whether the executable at `path` can run here: always
        /// `Ok` on this OS.
        pub async fn check(_path: &std::path::Path) -> Result<(), Incompatible> {
            Ok(())
        }
    }
}

/// Why a binary can't run on this system.
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Incompatible {
    /// The binary's dynamic loader (its `PT_INTERP`) isn't installed.
    Loader { interp: String },
    /// A shared library it needs isn't installed.
    Library { name: String },
    /// An installed library lacks a symbol version it needs, e.g. it
    /// needs `GLIBC_2.39` and the host's glibc is 2.35.
    Version { library: String, version: String },
}

impl fmt::Display for Incompatible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loader { interp } if interp.contains("ld-musl-") => write!(
                f,
                "its dynamic loader {interp} is missing (it is dynamically linked against musl libc)"
            ),
            Self::Loader { interp } => write!(f, "its dynamic loader {interp} is missing"),
            Self::Library { name } => write!(f, "it needs {name}, which is not installed"),
            Self::Version { library, version } => {
                write!(
                    f,
                    "it needs {version} from {library}, which this system lacks"
                )
            }
        }
    }
}

impl Error for Incompatible {}
