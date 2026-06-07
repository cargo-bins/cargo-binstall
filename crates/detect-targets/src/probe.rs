//! Empirical detection of runnable Linux targets by probing dynamic
//! loaders.
//!
//! For each known (architecture × libc) combination this module can
//! synthesize a minimal dynamically-linked executable whose
//! `PT_INTERP` is the ABI-standard loader path of that combination,
//! run it, and observe whether the kernel + loader can execute it.
//! This is the same mechanism any real downloaded binary uses, so the
//! result cannot disagree with reality — unlike heuristics based on
//! loader version banners, which distros rebrand (e.g. Gentoo), or on
//! filesystem paths, which vary.
//!
//! A probe for a non-native architecture succeeds if and only if the
//! system can actually run such binaries, e.g. multilib glibc for
//! `i686-unknown-linux-gnu` on x86_64, or a qemu-user binfmt handler
//! plus the matching libc for a fully foreign architecture.
//!
//! ```no_run
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! use detect_targets::probe::{find, ProbeResult};
//!
//! let probe = find("x86_64-unknown-linux-gnu").unwrap();
//! match probe.run().await {
//!     ProbeResult::Runnable => println!("glibc binaries run here"),
//!     ProbeResult::NotRunnable => println!("no usable glibc loader"),
//!     ProbeResult::Inconclusive(err) => println!("could not test: {err}"),
//! }
//! # }
//! ```

use std::{fs, io, os::unix::fs::PermissionsExt, process::Stdio};

use tokio::process::Command;
#[cfg(feature = "tracing")]
use tracing::debug;

mod elf;
mod table;

use elf::ElfSpec;

/// A loader probe attesting one Rust target triple.
pub struct Probe {
    /// The Rust target triple whose binaries this probe stands in for.
    pub target: &'static str,
    spec: ElfSpec,
}

/// The outcome of running a [`Probe`].
#[derive(Debug)]
pub enum ProbeResult {
    /// The probe executed and exited cleanly: binaries for this target
    /// run on this system.
    Runnable,
    /// The loader is missing (`ENOENT`), the architecture cannot be
    /// executed (`ENOEXEC`), or the loader failed to run the probe:
    /// binaries for this target do not run here.
    NotRunnable,
    /// The environment prevented the test from happening at all, e.g.
    /// a `noexec` temporary directory, SELinux, or seccomp. Nothing
    /// can be concluded about the target; callers should fall back to
    /// other detection methods.
    Inconclusive(io::Error),
}

/// All known probes.
pub fn probes() -> &'static [Probe] {
    table::PROBES
}

/// Find the probe attesting `target`, if one is known.
pub fn find(target: &str) -> Option<&'static Probe> {
    table::PROBES.iter().find(|p| p.target == target)
}

impl Probe {
    /// The synthesized probe executable.
    pub fn synthesize(&self) -> Vec<u8> {
        elf::synthesize(&self.spec)
    }

    /// Write the probe executable to a temporary directory and run it.
    pub async fn run(&self) -> ProbeResult {
        let progdir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => return ProbeResult::Inconclusive(err),
        };
        let prog = progdir.path().join(self.target);

        let setup = (|| {
            fs::write(&prog, self.synthesize())?;
            fs::set_permissions(&prog, fs::Permissions::from_mode(0o755))
        })();
        if let Err(err) = setup {
            return ProbeResult::Inconclusive(err);
        }

        let result = Command::new(&prog)
            .stdin(Stdio::null())
            .output()
            .await;

        match result {
            // The stub only ever calls exit_group(0); any output or
            // unclean exit is the loader complaining.
            Ok(out) => {
                #[cfg(feature = "tracing")]
                debug!(
                    "probe {}: status={:?}, stdout={:?}, stderr={:?}",
                    self.target,
                    out.status,
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr),
                );
                if out.status.success() && out.stdout.is_empty() && out.stderr.is_empty() {
                    ProbeResult::Runnable
                } else {
                    ProbeResult::NotRunnable
                }
            }
            Err(err) => {
                #[cfg(feature = "tracing")]
                debug!("probe {}: exec failed: {err:?}", self.target);
                match err.raw_os_error() {
                    // ENOENT: we just wrote the file, so the missing
                    // thing is the PT_INTERP loader.
                    Some(2) => ProbeResult::NotRunnable,
                    // ENOEXEC: no binfmt handler for this architecture.
                    Some(8) => ProbeResult::NotRunnable,
                    // Anything else (EACCES from a noexec mount, EPERM
                    // from seccomp, ...) blocked the test itself.
                    _ => ProbeResult::Inconclusive(err),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_targets_unique() {
        let mut targets: Vec<_> = probes().iter().map(|p| p.target).collect();
        targets.sort_unstable();
        let len = targets.len();
        targets.dedup();
        assert_eq!(len, targets.len());
    }

    /// The probe result for the native gnu target must agree with the
    /// presence of its loader, and must never be inconclusive in a
    /// regular test environment.
    #[tokio::test]
    async fn native_gnu_probe_agrees_with_loader_presence() {
        #[cfg(target_arch = "x86_64")]
        let target = "x86_64-unknown-linux-gnu";
        #[cfg(target_arch = "aarch64")]
        let target = "aarch64-unknown-linux-gnu";
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        return;

        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        {
            let probe = find(target).unwrap();
            let loader_exists = std::path::Path::new(probe.spec.interp).exists();
            match probe.run().await {
                ProbeResult::Runnable => assert!(loader_exists),
                ProbeResult::NotRunnable => assert!(!loader_exists),
                ProbeResult::Inconclusive(err) => panic!("probe inconclusive: {err}"),
            }
        }
    }

    /// No system has a uClibc loader unless deeply embedded; this
    /// exercises the NotRunnable path on any CI host.
    #[tokio::test]
    async fn uclibc_probe_not_runnable() {
        if std::path::Path::new("/lib/ld-uClibc.so.0").exists() {
            return;
        }
        let probe = find("armv7-unknown-linux-uclibceabihf").unwrap();
        match probe.run().await {
            ProbeResult::NotRunnable => (),
            res => panic!("expected NotRunnable, got {res:?}"),
        }
    }
}
