//! The check on Linux and Android.

use std::path::Path;

use tokio::{sync::OnceCell, task::spawn_blocking};
#[cfg(feature = "tracing")]
use tracing::debug;

use super::{
    Incompatible,
    elf::{self, Elf, Ident},
    search::{self, Flavor, SystemPath},
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
    /// A host nothing is known about yet: probes run on demand, see
    /// [`Self::run_probe`].
    pub(crate) fn new() -> Self {
        Self {
            probes: probe::probes().iter().map(|_| OnceCell::new()).collect(),
        }
    }

    /// Run the dynamic (`PT_INTERP`) probe `probe` once for this host:
    /// `Some(runnable)`, or `None` if inconclusive.
    pub(crate) async fn run_probe(&self, probe: &'static Probe) -> Option<bool> {
        let index = probe::probes()
            .iter()
            .position(|p| std::ptr::eq(p, probe))
            .expect("probes come from the probe table");
        *self.probes[index]
            .get_or_init(|| async {
                match probe.run().await {
                    ProbeResult::Runnable => Some(true),
                    ProbeResult::NotRunnable => Some(false),
                    ProbeResult::Inconclusive(_err) => {
                        #[cfg(feature = "tracing")]
                        debug!("probe {} inconclusive: {_err}", probe.target);
                        None
                    }
                }
            })
            .await
    }

    /// Check whether the executable at `path` can run on this system.
    ///
    /// Returns `Ok` if it can, or if that can't be determined, including
    /// when `path` is not an ELF file or can't be read.
    pub async fn check(&self, path: &Path) -> Result<(), Incompatible> {
        // The blocking parts run on the blocking pool. A `JoinError` from
        // it, i.e. a panic, skips the check, but only in builds that
        // unwind: cargo-binstall's release builds abort on panic.
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

        spawn_blocking(move || check_libraries(&elf, &interp))
            .await
            .unwrap_or(Ok(()))
    }

    /// Whether the loader at `interp` can run programs of this identity.
    async fn loader_runs(&self, interp: &str, ident: Ident) -> bool {
        match find_probe(interp, ident) {
            Some(probe) => match self.run_probe(probe).await {
                Some(runnable) => runnable,
                None => Path::new(interp).is_file(),
            },
            // Not a standard loader path, so there is no probe for it.
            // Running a probe with an arbitrary `PT_INTERP` would run
            // whatever program the archive names, so just check that it
            // exists.
            None => Path::new(interp).is_file(),
        }
    }
}

fn find_probe(interp: &str, ident: Ident) -> Option<&'static Probe> {
    probe::probes().iter().find(|p| {
        p.spec.interp == interp
            && p.spec.class == ident.class
            && p.spec.endian == ident.endian
            && p.spec.e_machine == ident.machine
    })
}

/// The identity of the target this was built for, for telling native
/// binaries from ones run through compat mode or binfmt. `None` for a
/// target without a probe, which is never sure.
fn host_ident() -> Option<Ident> {
    probe::find(crate::TARGET).map(|probe| Ident {
        class: probe.spec.class,
        endian: probe.spec.endian,
        machine: probe.spec.e_machine,
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

/// Check the libraries `elf` needs against those installed.
fn check_libraries(elf: &Elf, interp: &str) -> Result<(), Incompatible> {
    let Some(flavor) = Flavor::of(interp) else {
        // bionic, uClibc, ...: only the loader was checked.
        return Ok(());
    };
    let musl = matches!(flavor, Flavor::Musl { .. });

    // A library not found may still be found by the loader when the
    // binary is for another architecture, e.g. run through qemu-user,
    // whose libraries may live in a sysroot only the emulator knows
    // about. The search itself also doesn't conclude on what it can't
    // follow: rpaths, and configuration it can't read.
    let can_tell_missing = host_ident() == Some(elf.ident);
    let libraries = search::libraries(elf, &SystemPath::read(&flavor), can_tell_missing, |name| {
        musl && is_musl_builtin(name)
    });
    if let Some(name) = libraries.missing {
        return Err(Incompatible::Library { name });
    }

    // musl doesn't implement symbol versioning.
    if musl {
        return Ok(());
    }

    for verneed in &elf.verneed {
        let Some(lib) = libraries.found.get(&verneed.file) else {
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

    /// This test binary runs, so it must pass the check.
    #[tokio::test]
    async fn own_executable_compatible() {
        check(&std::env::current_exe().unwrap()).await.unwrap();
    }

    /// A copy of this test binary with `from` replaced by `to`, which
    /// must be no longer, NUL-padded to keep every offset in place.
    #[cfg(all(
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    fn patched_exe(dir: &Path, from: &[u8], to: &[u8]) -> std::path::PathBuf {
        assert!(to.len() <= from.len());
        let mut to = to.to_vec();
        to.resize(from.len(), 0);

        let mut exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
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

    /// Detection runs the native glibc loader probe, and keeps its
    /// result for checking binaries.
    #[tokio::test]
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    async fn detection_records_native_loader() {
        let host = crate::detect().await.host;
        let target = format!("{}-unknown-linux-gnu", std::env::consts::ARCH);
        let index = probe::probes()
            .iter()
            .position(|p| p.target == target)
            .unwrap();
        assert!(host.probes[index].initialized(), "{target}");
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
