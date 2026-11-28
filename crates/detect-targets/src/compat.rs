//! Check that a downloaded executable can run on this system, before it
//! is installed.
//!
//! Detecting the host's targets says which *kinds* of binaries run here,
//! but a given binary can still need a dynamic loader the host lacks (a
//! musl binary linked dynamically on a glibc host), or a newer glibc than
//! the host's. [`check`] reads both from the binary without running
//! anything. Shared libraries other than glibc aren't checked: the user
//! may be expected to install them. Binaries for another architecture
//! aren't checked either, as an emulator may give them a sysroot of their
//! own. When something can't be determined, the binary passes. The check
//! only does something on Linux and Android.

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
    /// The binary needs a newer `GLIBC_*` version than the host's
    /// `libc.so.6` defines, e.g. `needed` is `2.39` and `available` is
    /// `2.35`.
    Glibc { needed: String, available: String },
}

impl fmt::Display for Incompatible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loader { interp } if interp.contains("ld-musl-") => write!(
                f,
                "its dynamic loader {interp} is missing (it is dynamically linked against musl libc)"
            ),
            Self::Loader { interp } => write!(f, "its dynamic loader {interp} is missing"),
            Self::Glibc { needed, available } => write!(
                f,
                "it needs GLIBC_{needed}, but the newest this system has is GLIBC_{available}"
            ),
        }
    }
}

impl Error for Incompatible {}
