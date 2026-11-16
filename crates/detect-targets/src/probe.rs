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

use std::{fs::Permissions, io, os::unix::fs::PermissionsExt, path::Path, process::Stdio};

use tokio::{
    fs,
    process::{Child, Command},
    sync::Mutex,
};
#[cfg(feature = "tracing")]
use tracing::debug;

mod elf;
mod memfd;
mod table;

use elf::ElfSpec;
use memfd::SealedExecutable;

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
    /// memfds are disabled and the temporary directory is `noexec`, or
    /// SELinux or seccomp blocked the exec. Nothing
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
    /// The synthesized dynamically-linked probe executable.
    pub fn synthesize(&self) -> Vec<u8> {
        elf::synthesize(&self.spec, true)
    }

    /// The synthesized static probe executable (no `PT_INTERP`).
    pub fn synthesize_static(&self) -> Vec<u8> {
        elf::synthesize(&self.spec, false)
    }

    /// Run the dynamically-linked probe executable.
    ///
    /// See [`Self::run_static`] for where the executable is stored.
    ///
    /// [`ProbeResult::Runnable`] proves both that the kernel can
    /// execute this architecture and that the dynamic loader of this
    /// target's libc is present and working — i.e. dynamically-linked
    /// binaries for this target run here.
    pub async fn run(&self) -> ProbeResult {
        self.exec(self.synthesize()).await
    }

    /// Run the static probe executable.
    ///
    /// The executable is written to a sealed `memfd` and run through
    /// `/proc/self/fd`. If memfds are unavailable, can't be executed, or
    /// `/proc` isn't mounted, it is written to a temporary directory
    /// instead.
    ///
    /// [`ProbeResult::Runnable`] proves only that the kernel can
    /// execute binaries of this architecture — natively, via compat
    /// mode (e.g. ia32 on x86_64, aarch32 on arm64), or through a
    /// binfmt handler. That is the requirement for *statically*
    /// linked binaries, which Rust musl artifacts typically are.
    pub async fn run_static(&self) -> ProbeResult {
        self.exec(self.synthesize_static()).await
    }

    async fn exec(&self, executable: Vec<u8>) -> ProbeResult {
        // Serializes the write-then-spawn sections of concurrent
        // probes. A fork performed while another probe's file is
        // still open for writing leaves a copy of the write fd in the
        // forked child until its execve completes, making the exec of
        // that other probe fail with ETXTBSY. Linux 6.18 lets a memfd
        // run while it is open for writing, but older kernels may not,
        // and the temporary directory fallback needs the lock anyway.
        static EXEC_LOCK: Mutex<()> = Mutex::const_new(());

        let spawned = {
            let _guard = EXEC_LOCK.lock().await;

            match self.spawn_from_memfd(&executable).await {
                Some(spawned) => spawned,
                None => match self.spawn_from_tempdir(executable).await {
                    Ok(spawned) => spawned,
                    Err(err) => return ProbeResult::Inconclusive(err),
                },
            }
        };

        let result = match spawned {
            Ok(child) => child.wait_with_output().await,
            Err(err) => Err(err),
        };

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
                    // ENOENT: we just wrote the file (and checked that
                    // its `/proc` path resolves), so the missing thing
                    // is the PT_INTERP loader.
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

    /// Spawn the probe from a sealed memfd, or return `None` if that
    /// isn't possible here and a temporary directory should be tried.
    async fn spawn_from_memfd(&self, executable: &[u8]) -> Option<io::Result<Child>> {
        let memfd = match SealedExecutable::new(self.target, executable) {
            Ok(memfd) => memfd,
            Err(_err) => {
                #[cfg(feature = "tracing")]
                debug!("probe {}: memfd unavailable: {_err}", self.target);
                return None;
            }
        };

        match spawn(memfd.path()).await {
            // EACCES: the kernel or an LSM refused to execute a memfd,
            // but a file on disk may still be allowed.
            Err(err) if err.raw_os_error() == Some(13) => {
                #[cfg(feature = "tracing")]
                debug!("probe {}: memfd not executable: {err}", self.target);
                None
            }
            // The spawn only returns once the child has exec'd, so the
            // memfd can be dropped now.
            spawned => Some(spawned),
        }
    }

    /// Spawn the probe from a file in a fresh temporary directory.
    ///
    /// The outer error means the file could not be written, and so
    /// nothing was tested.
    async fn spawn_from_tempdir(&self, executable: Vec<u8>) -> io::Result<io::Result<Child>> {
        let progdir = tempfile::tempdir()?;
        let prog = progdir.path().join(self.target);

        // `tokio::fs` runs these on the blocking pool, and closes the
        // file before returning, so the spawn below can't race with
        // our own write fd.
        fs::write(&prog, executable).await?;
        fs::set_permissions(&prog, Permissions::from_mode(0o755)).await?;

        // The spawn only returns once the child has exec'd, so the
        // directory can be removed now.
        Ok(spawn(&prog).await)
    }
}

async fn spawn(prog: &Path) -> io::Result<Child> {
    let mut command = Command::new(prog);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // The lock does not cover forks performed elsewhere in the
    // process, so a residual ETXTBSY is still possible; it resolves
    // as soon as the offending child execs, so retry after yielding.
    let mut attempts = 0;
    loop {
        match command.spawn() {
            // ETXTBSY
            Err(err) if err.raw_os_error() == Some(26) && attempts < 10 => {
                attempts += 1;
                tokio::task::yield_now().await;
            }
            spawned => break spawned,
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

    /// The static probe for the native architecture must always run:
    /// the kernel can execute its own architecture by definition.
    #[tokio::test]
    async fn native_static_probe_runnable() {
        #[cfg(target_arch = "x86_64")]
        let target = "x86_64-unknown-linux-musl";
        #[cfg(target_arch = "aarch64")]
        let target = "aarch64-unknown-linux-musl";
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        return;

        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        match find(target).unwrap().run_static().await {
            ProbeResult::Runnable => (),
            res => panic!("expected Runnable, got {res:?}"),
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
