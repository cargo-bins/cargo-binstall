//! The check on Linux and Android.

use std::{
    env,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use lddtree::DependencyAnalyzer;
use tokio::{sync::OnceCell, task::spawn_blocking};
#[cfg(feature = "tracing")]
use tracing::debug;

use super::{
    Incompatible,
    elf::{self, Elf, Ident},
    file_name,
};
use crate::probe::{self, Probe, ProbeResult};

/// What target detection found out about this host, to check binaries
/// against: the loader probes it ran.
#[derive(Debug)]
pub struct Host {
    /// Results of the probes run so far, in [`probe::probes`] order:
    /// `Some(runnable)`, or `None` if inconclusive.
    probes: Vec<OnceCell<Option<bool>>>,
}

impl Host {
    /// A host nothing is known about yet. Probes are recorded with
    /// [`Self::record`] as detection runs them, and run on demand by
    /// [`Self::check`] otherwise.
    pub(crate) fn new() -> Self {
        Self {
            probes: probe::probes().iter().map(|_| OnceCell::new()).collect(),
        }
    }

    /// Record the result of a dynamic (`PT_INTERP`) probe run during
    /// detection, so that checking a binary for that loader reuses it.
    pub(crate) fn record(&self, probe: &Probe, result: &ProbeResult) {
        if let Some(index) = probe::probes().iter().position(|p| std::ptr::eq(p, probe)) {
            let _ = self.probes[index].set(summarize(probe, result));
        }
    }

    /// Check whether the executable at `path` can run on this system.
    ///
    /// Returns `Ok` if it can, or if that can't be determined, including
    /// when `path` is not an ELF file or can't be read.
    pub async fn check(&self, path: &Path) -> Result<(), Incompatible> {
        let elf = {
            let path = path.to_owned();
            spawn_blocking(move || elf::parse(&path)).await.ok()
        };
        let elf = match elf {
            Some(Ok(Some(elf))) => elf,
            _other => {
                #[cfg(feature = "tracing")]
                debug!(
                    "compat {}: skipped, not a readable ELF: {_other:?}",
                    path.display()
                );
                return Ok(());
            }
        };
        #[cfg(feature = "tracing")]
        debug!("compat {}: {elf:?}", path.display());

        // A static executable only needs the kernel to support its
        // architecture, which target detection already established.
        let Some(interp) = elf.interp.clone() else {
            return Ok(());
        };

        if !self.loader_runs(&interp, elf.ident).await {
            return Err(Incompatible::Loader { interp });
        }

        let path = path.to_owned();
        spawn_blocking(move || check_libraries(&path, &elf, &interp))
            .await
            .unwrap_or(Ok(()))
    }

    /// Whether the loader at `interp` can run programs of this identity.
    async fn loader_runs(&self, interp: &str, ident: Ident) -> bool {
        match find_probe(interp, ident) {
            Some((index, probe)) => {
                let runnable = self.probes[index]
                    .get_or_init(|| async { summarize(probe, &probe.run().await) })
                    .await;
                match runnable {
                    Some(runnable) => *runnable,
                    None => Path::new(interp).is_file(),
                }
            }
            // Not a standard loader path, so there is no probe for it.
            // Running a probe with an arbitrary `PT_INTERP` would run
            // whatever program the archive names, so just check that it
            // exists.
            None => Path::new(interp).is_file(),
        }
    }
}

fn summarize(_probe: &Probe, result: &ProbeResult) -> Option<bool> {
    match result {
        ProbeResult::Runnable => Some(true),
        ProbeResult::NotRunnable => Some(false),
        ProbeResult::Inconclusive(_err) => {
            #[cfg(feature = "tracing")]
            debug!("compat: probe {} inconclusive: {_err}", _probe.target);
            None
        }
    }
}

fn find_probe(interp: &str, ident: Ident) -> Option<(usize, &'static Probe)> {
    probe::probes().iter().enumerate().find(|(_, p)| {
        p.spec.interp == interp
            && p.spec.class == ident.class
            && p.spec.endian == ident.endian
            && p.spec.e_machine == ident.machine
    })
}

/// How a loader looks up libraries.
enum Flavor {
    Glibc,
    Musl,
}

fn flavor(interp: &str) -> Option<Flavor> {
    let name = file_name(interp);
    if name.starts_with("ld-musl-") {
        Some(Flavor::Musl)
    } else if name.starts_with("ld-linux") || name.starts_with("ld64.so.") || name == "ld.so.1" {
        Some(Flavor::Glibc)
    } else {
        None
    }
}

/// The identity of this process's own executable, for telling native
/// binaries from ones run through compat mode or binfmt.
fn host_ident() -> Option<Ident> {
    static HOST: OnceLock<Option<Ident>> = OnceLock::new();
    *HOST.get_or_init(|| {
        let exe = env::current_exe().ok()?;
        Some(elf::parse(&exe).ok()??.ident)
    })
}

/// musl's loader provides these itself: `libc.so`, `libpthread.so.0`, ...
fn is_musl_builtin(name: &str) -> bool {
    name.strip_prefix("lib")
        .and_then(|rest| rest.split_once('.'))
        .is_some_and(|(lib, _)| {
            matches!(lib, "c" | "pthread" | "rt" | "m" | "dl" | "util" | "xnet")
        })
}

/// Check the libraries the binary at `path` needs, `elf`, against those
/// installed.
fn check_libraries(path: &Path, elf: &Elf, interp: &str) -> Result<(), Incompatible> {
    let Some(flavor) = flavor(interp) else {
        // bionic, uClibc, ...: only the loader was checked.
        return Ok(());
    };
    let musl = matches!(flavor, Flavor::Musl);

    // Find every library the loader would load, recursively, the way it
    // would: rpath/runpath, `LD_LIBRARY_PATH`, then `/etc/ld.so.conf` or
    // musl's `/etc/ld-musl-$ARCH.path`, then the default directories.
    let tree = match DependencyAnalyzer::new(PathBuf::from("/")).analyze(path) {
        Ok(tree) => tree,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            debug!("compat {}: can't resolve libraries: {_err}", path.display());
            return Ok(());
        }
    };

    // A library not found may still be found by the loader when:
    //
    // - the binary is for another architecture, e.g. run through
    //   qemu-user, whose libraries may live in a sysroot only the
    //   emulator knows about;
    // - an rpath depends on where the binary ends up (`$ORIGIN` and
    //   friends), not where it was downloaded to;
    // - glibc is configured without `/etc/ld.so.conf`, e.g. NixOS'
    //   nix-ld, which takes its library path from the environment.
    let can_tell_missing = host_ident() == Some(elf.ident)
        && !elf.rpaths.iter().any(|rpath| rpath.contains('$'))
        && (musl || Path::new("/etc/ld.so.conf").exists());

    let mut missing: Vec<_> = tree
        .libraries
        .values()
        .filter(|lib| !lib.found() && !(musl && is_musl_builtin(&lib.name)))
        .map(|lib| &lib.name)
        .collect();
    missing.sort_unstable();
    if let Some(name) = missing.first() {
        if can_tell_missing {
            return Err(Incompatible::Library {
                name: (*name).clone(),
            });
        }
        #[cfg(feature = "tracing")]
        debug!("compat: {missing:?} not found, but the loader may find them");
    }

    // musl doesn't implement symbol versioning.
    if musl {
        return Ok(());
    }

    for verneed in &elf.verneed {
        let Some(lib) = tree
            .libraries
            .get(&verneed.file)
            .and_then(|lib| lib.realpath.as_deref())
        else {
            continue;
        };
        let Some(provided) = elf::parse(lib).ok().flatten().map(|lib| lib.verdef) else {
            continue;
        };
        // glibc only warns about a library without version definitions.
        if provided.is_empty() {
            continue;
        }

        let missing = verneed
            .versions
            .iter()
            .filter(|(version, weak)| !weak && !provided.contains(version))
            .map(|(version, _)| version)
            .max_by(|a, b| version_cmp(a, b));
        if let Some(version) = missing {
            let series = series(version);
            let newest = provided
                .iter()
                .filter(|v| series.is_some() && series == self::series(v))
                .max_by(|a, b| version_cmp(a, b))
                .cloned();
            return Err(Incompatible::Version {
                library: verneed.file.clone(),
                version: version.clone(),
                provided: newest,
            });
        }
    }

    Ok(())
}

/// The series of a version name: `GLIBC_` for `GLIBC_2.39`.
fn series(version: &str) -> Option<&str> {
    let (series, number) = version.rsplit_once('_')?;
    number
        .split('.')
        .all(|n| n.parse::<u64>().is_ok())
        .then_some(series)
}

/// Order version names by series, then numerically: `GLIBC_2.9` comes
/// before `GLIBC_2.10`.
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |v: &str| -> (String, Vec<u64>) {
        match v.rsplit_once('_') {
            Some((series, number)) => (
                series.to_owned(),
                number.split('.').map(|n| n.parse().unwrap_or(0)).collect(),
            ),
            None => (v.to_owned(), Vec::new()),
        }
    };
    key(a).cmp(&key(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn check(path: &Path) -> Result<(), Incompatible> {
        Host::new().check(path).await
    }

    #[test]
    fn version_order() {
        assert!(version_cmp("GLIBC_2.9", "GLIBC_2.10").is_lt());
        assert!(version_cmp("GLIBC_2.39", "GLIBC_2.4").is_gt());
        assert!(version_cmp("GLIBC_2.2.5", "GLIBC_2.3").is_lt());
        assert_eq!(series("GLIBC_2.39"), Some("GLIBC"));
        assert_eq!(series("GLIBCXX_3.4.32"), Some("GLIBCXX"));
        assert_eq!(series("GLIBC_PRIVATE"), None);
    }

    #[test]
    fn musl_builtins() {
        assert!(is_musl_builtin("libc.so"));
        assert!(is_musl_builtin("libc.musl-x86_64.so.1"));
        assert!(is_musl_builtin("libpthread.so.0"));
        assert!(is_musl_builtin("libm.so.6"));
        assert!(!is_musl_builtin("libgcc_s.so.1"));
        assert!(!is_musl_builtin("libcrypto.so.3"));
        assert!(!is_musl_builtin("libmagic.so.1"));
    }

    #[test]
    fn flavors() {
        assert!(matches!(
            flavor("/lib64/ld-linux-x86-64.so.2"),
            Some(Flavor::Glibc)
        ));
        assert!(matches!(
            flavor("/lib/ld-linux-aarch64.so.1"),
            Some(Flavor::Glibc)
        ));
        assert!(matches!(flavor("/lib64/ld64.so.2"), Some(Flavor::Glibc)));
        assert!(matches!(
            flavor("/lib/ld-musl-x86_64.so.1"),
            Some(Flavor::Musl)
        ));
        assert!(flavor("/system/bin/linker64").is_none());
    }

    /// This test binary runs, so it must pass the check.
    #[tokio::test]
    async fn own_executable_compatible() {
        check(&env::current_exe().unwrap()).await.unwrap();
    }

    /// A copy of this test binary with `from` replaced by `to`, which
    /// must be no longer, NUL-padded to keep every offset in place.
    #[cfg(all(
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    fn patched_exe(dir: &Path, from: &[u8], to: &[u8]) -> PathBuf {
        assert!(to.len() <= from.len());
        let mut to = to.to_vec();
        to.resize(from.len(), 0);

        let mut exe = std::fs::read(env::current_exe().unwrap()).unwrap();
        let mut replaced = 0;
        let mut i = 0;
        while let Some(pos) = exe[i..].windows(from.len()).position(|w| w == from) {
            exe[i + pos..i + pos + from.len()].copy_from_slice(&to);
            i += pos + from.len();
            replaced += 1;
        }
        assert!(replaced > 0);

        let path = dir.join("patched");
        std::fs::write(&path, exe).unwrap();
        path
    }

    #[cfg(all(target_env = "gnu", target_arch = "x86_64"))]
    const GLIBC_LOADER: &[u8] = b"/lib64/ld-linux-x86-64.so.2\0";
    #[cfg(all(target_env = "gnu", target_arch = "aarch64"))]
    const GLIBC_LOADER: &[u8] = b"/lib/ld-linux-aarch64.so.1\0";
    #[cfg(all(target_env = "gnu", target_arch = "x86_64"))]
    const MUSL_LOADER: &[u8] = b"/lib/ld-musl-x86_64.so.1\0";
    #[cfg(all(target_env = "gnu", target_arch = "aarch64"))]
    const MUSL_LOADER: &[u8] = b"/lib/ld-musl-aarch64.so.1\0";

    /// A binary needing a newer glibc than the host's is rejected, and
    /// the error names the host's newest version.
    #[tokio::test]
    #[cfg(all(
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    async fn newer_glibc_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let exe = patched_exe(dir.path(), b"GLIBC_2.", b"GLIBC_9.");
        match check(&exe).await {
            Err(Incompatible::Version {
                library,
                version,
                provided: Some(provided),
            }) => {
                // Whichever glibc library comes first among the
                // binary's version requirements: libc, libm, ...
                assert!(library.contains(".so"), "{library}");
                assert!(version.starts_with("GLIBC_9."), "{version}");
                assert!(provided.starts_with("GLIBC_2."), "{provided}");
            }
            res => panic!("expected a version error, got {res:?}"),
        }
    }

    /// A binary linking to a library the host lacks is rejected.
    #[tokio::test]
    #[cfg(all(
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    async fn missing_library_rejected() {
        if !Path::new("/etc/ld.so.conf").exists() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let exe = patched_exe(dir.path(), b"libc.so.6\0", b"libQ.so.6\0");
        assert_eq!(
            check(&exe).await,
            Err(Incompatible::Library {
                name: "libQ.so.6".into()
            })
        );
    }

    /// A binary dynamically linked against musl is rejected on a host
    /// without the musl loader (#1466), and so is one whose loader is
    /// at a path that doesn't exist.
    #[tokio::test]
    #[cfg(all(
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    async fn missing_loader_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let musl = std::str::from_utf8(&MUSL_LOADER[..MUSL_LOADER.len() - 1]).unwrap();
        if !Path::new(musl).exists() {
            let exe = patched_exe(dir.path(), GLIBC_LOADER, MUSL_LOADER);
            assert_eq!(
                check(&exe).await,
                Err(Incompatible::Loader {
                    interp: musl.into()
                })
            );
        }

        let exe = patched_exe(dir.path(), GLIBC_LOADER, b"/nonexistent/ld.so\0");
        assert_eq!(
            check(&exe).await,
            Err(Incompatible::Loader {
                interp: "/nonexistent/ld.so".into()
            })
        );
    }

    /// Detection runs the native glibc and musl loader probes, and keeps
    /// their results for checking binaries.
    #[tokio::test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    async fn detection_records_native_loaders() {
        let host = crate::detect().await.host;
        let arch = std::env::consts::ARCH;
        for target in [
            format!("{arch}-unknown-linux-gnu"),
            format!("{arch}-unknown-linux-musl"),
        ] {
            let index = probe::probes()
                .iter()
                .position(|p| p.target == target)
                .unwrap();
            assert!(host.probes[index].initialized(), "{target}");
        }
    }

    /// Targets given explicitly are not checked.
    #[tokio::test]
    #[cfg(all(
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    async fn explicit_targets_not_checked() {
        let dir = tempfile::tempdir().unwrap();
        let exe = patched_exe(dir.path(), b"GLIBC_2.", b"GLIBC_9.");
        assert!(check(&exe).await.is_err());

        let targets = crate::get_desired_targets(Some(vec![crate::TARGET.into()]));
        targets.check_binary(&exe).await.unwrap();
    }

    /// Not an ELF file: nothing to check.
    #[tokio::test]
    async fn non_elf_skipped() {
        check(Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/Cargo.toml"
        )))
        .await
        .unwrap();
        check(Path::new("/nonexistent/binary")).await.unwrap();
    }
}
