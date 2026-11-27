//! Check that a downloaded executable can run on this system, without
//! running it.
//!
//! Detecting the host's targets (see [`crate::detect_targets`]) says
//! which *kinds* of binaries run here, but a given binary can still need
//! more than its target implies:
//!
//! - a dynamic loader the host lacks. Rust's musl targets link statically
//!   today, so they are offered on every Linux host, but a musl binary
//!   linked dynamically (`-C target-feature=-crt-static`, or a future
//!   default) needs `/lib/ld-musl-$ARCH.so.1`, which glibc hosts don't
//!   have;
//! - shared libraries the host doesn't have installed;
//! - newer versions of those libraries than the host has, most commonly a
//!   gnu binary built against a newer glibc than the host's.
//!
//! [`Host::check`] reads the binary's `PT_INTERP` and dynamic section, the
//! same things the loader looks at, and checks them against the host the
//! way the loader would. The loader is tested by running the matching
//! probe, never the binary itself, as nothing downloaded may run before
//! the user has confirmed the installation. Libraries are looked up the
//! way glibc's or musl's loader would look them up.
//!
//! The check only fails on what the loader would certainly reject. When
//! something can't be determined (a binary or library with an rpath, a
//! glibc without `/etc/ld.so.conf`, a foreign architecture run through
//! binfmt, a file that isn't ELF), that part is skipped.
//!
//! A [`Host`] comes from [`crate::detect`], and
//! [`DesiredTargets::check_binary`](crate::DesiredTargets::check_binary)
//! uses the one its detection produced. The check only does something on
//! Linux and Android; elsewhere every binary passes.

use std::{error::Error, fmt};

cfg_select! {
    any(target_os = "linux", target_os = "android") => {
        mod elf;
        mod linux;
        mod search;
        pub use linux::Host;
    }
    _ => {
        /// What target detection found out about this host, to check
        /// binaries against. Off Linux there is nothing to check.
        #[derive(Debug, Default)]
        pub struct Host(());

        impl Host {
            /// Check whether the executable at `path` can run here: always
            /// `Ok` on this OS.
            pub async fn check(&self, _path: &std::path::Path) -> Result<(), Incompatible> {
                Ok(())
            }
        }
    }
}

/// Why a binary can't run on this system.
#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Incompatible {
    /// The binary's dynamic loader (its `PT_INTERP`) is missing, or
    /// can't run here.
    Loader { interp: String },
    /// A shared library the binary links to isn't installed.
    Library { name: String },
    /// A library is installed, but doesn't provide a symbol version the
    /// binary requires, e.g. the binary needs `GLIBC_2.39` and the host's
    /// glibc is 2.35.
    Version {
        library: String,
        version: String,
        /// The newest version of the same series the host's library
        /// provides, if any, e.g. `GLIBC_2.35`.
        provided: Option<String>,
    },
}

impl fmt::Display for Incompatible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loader { interp } => {
                write!(
                    f,
                    "its dynamic loader {interp} is missing or can't run here"
                )?;
                if is_musl_loader(interp) {
                    f.write_str(" (it is dynamically linked against musl libc)")?;
                }
                Ok(())
            }
            Self::Library { name } => write!(f, "it needs {name}, which is not installed"),
            Self::Version {
                library,
                version,
                provided: Some(provided),
            } => write!(
                f,
                "it needs {version} from {library}, but this system only has up to {provided}"
            ),
            Self::Version {
                library,
                version,
                provided: None,
            } => write!(
                f,
                "it needs {version} from {library}, which this system lacks"
            ),
        }
    }
}

impl Error for Incompatible {}

fn is_musl_loader(interp: &str) -> bool {
    file_name(interp).starts_with("ld-musl-")
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(
            Incompatible::Loader {
                interp: "/lib/ld-musl-x86_64.so.1".into()
            }
            .to_string(),
            "its dynamic loader /lib/ld-musl-x86_64.so.1 is missing or can't run here \
             (it is dynamically linked against musl libc)"
        );
        assert_eq!(
            Incompatible::Version {
                library: "libc.so.6".into(),
                version: "GLIBC_2.39".into(),
                provided: Some("GLIBC_2.35".into()),
            }
            .to_string(),
            "it needs GLIBC_2.39 from libc.so.6, but this system only has up to GLIBC_2.35"
        );
    }
}
