//! The check on Linux and Android.

use std::{
    env,
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use tokio::{sync::OnceCell, task::spawn_blocking};
#[cfg(feature = "tracing")]
use tracing::debug;

use super::{
    Incompatible,
    elf::{self, Elf, Ident},
    file_name, ldcache,
};
use crate::probe::{self, Probe, ProbeResult};

/// What target detection found out about this host, to check binaries
/// against: the loader probes it ran and the libraries `ldconfig` knows.
#[derive(Debug)]
pub struct Host {
    /// Results of the probes run so far, in [`probe::probes`] order:
    /// `Some(runnable)`, or `None` if inconclusive.
    probes: Vec<OnceCell<Option<bool>>>,
    /// `/etc/ld.so.cache`, or `None` if it can't be read.
    ld_cache: Option<Arc<[(String, PathBuf)]>>,
}

impl Host {
    /// Read what doesn't need a probe. Probes are recorded with
    /// [`Self::record`] as detection runs them, and run on demand by
    /// [`Self::check`] otherwise.
    pub(crate) async fn load() -> Self {
        let ld_cache = spawn_blocking(|| match ldcache::read(ldcache::PATH) {
            Ok(cache) => Some(Arc::from(cache)),
            Err(_err) => {
                #[cfg(feature = "tracing")]
                debug!("compat: can't read {}: {_err}", ldcache::PATH);
                None
            }
        })
        .await
        .ok()
        .flatten();

        Self {
            probes: probe::probes().iter().map(|_| OnceCell::new()).collect(),
            ld_cache,
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
            spawn_blocking(move || open_regular(&path).and_then(|file| elf::parse(&file)))
                .await
                .ok()
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

        let ld_cache = self.ld_cache.clone();
        spawn_blocking(move || check_libraries(&elf, &interp, ld_cache.as_deref()))
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

/// Open `path` if it is a regular file: opening a FIFO, which an archive
/// may link to, would block.
fn open_regular(path: &Path) -> std::io::Result<File> {
    if fs::metadata(path)?.is_file() {
        File::open(path)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
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
    /// musl, with the `$ARCH` from its loader name `ld-musl-$ARCH.so.1`.
    Musl(String),
}

fn flavor(interp: &str) -> Option<Flavor> {
    let name = file_name(interp);
    if let Some(arch) = name
        .strip_prefix("ld-musl-")
        .and_then(|rest| rest.strip_suffix(".so.1"))
    {
        Some(Flavor::Musl(arch.to_owned()))
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
        elf::read_ident(&File::open(exe).ok()?).ok()?
    })
}

/// Where the loader would look for the libraries `elf` needs.
struct SearchPath<'a> {
    /// Directories searched before the cache.
    dirs: Vec<PathBuf>,
    /// `/etc/ld.so.cache`, consulted after `dirs`.
    cache: Option<&'a [(String, PathBuf)]>,
    /// Directories searched last.
    defaults: Vec<PathBuf>,
    /// Whether some directory couldn't be determined, so that a library
    /// not found may still be found by the loader.
    incomplete: bool,
}

impl<'a> SearchPath<'a> {
    fn new(
        elf: &Elf,
        interp: &str,
        flavor: &Flavor,
        ld_cache: Option<&'a [(String, PathBuf)]>,
    ) -> Self {
        let mut path = Self {
            dirs: Vec::new(),
            cache: None,
            defaults: Vec::new(),
            incomplete: false,
        };

        let ld_library_path = env::var("LD_LIBRARY_PATH").ok();
        match flavor {
            Flavor::Glibc => {
                // DT_RPATH is ignored when DT_RUNPATH is present.
                if elf.runpath.is_none() {
                    path.push_list(elf.rpath.as_deref());
                }
                path.push_list(ld_library_path.as_deref());
                path.push_list(elf.runpath.as_deref());

                path.cache = ld_cache;
                if ld_cache.is_none() {
                    path.incomplete = true;
                }

                // glibc's built-in directories; distros using a multiarch
                // layout add theirs, which is where the loader lives.
                if let Some(dir) = fs::canonicalize(interp)
                    .ok()
                    .and_then(|interp| Some(interp.parent()?.to_owned()))
                {
                    path.defaults.push(dir);
                }
                if elf.ident.class == probe::elf::Class::Elf64 {
                    path.defaults.extend(["/lib64".into(), "/usr/lib64".into()]);
                }
                path.defaults.extend(["/lib".into(), "/usr/lib".into()]);
            }
            Flavor::Musl(arch) => {
                path.push_list(ld_library_path.as_deref());
                path.push_list(elf.rpath.as_deref());
                path.push_list(elf.runpath.as_deref());

                match fs::read_to_string(format!("/etc/ld-musl-{arch}.path")) {
                    Ok(list) => path.defaults.extend(
                        list.split(['\n', ':'])
                            .filter(|d| !d.is_empty())
                            .map(Into::into),
                    ),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        path.defaults
                            .extend(["/lib", "/usr/local/lib", "/usr/lib"].map(PathBuf::from));
                    }
                    Err(_) => path.incomplete = true,
                }
            }
        }

        path
    }

    fn push_list(&mut self, list: Option<&str>) {
        for dir in list.into_iter().flat_map(|list| list.split(':')) {
            // `$ORIGIN` and friends depend on where the binary ends up,
            // and relative entries on the working directory at run time.
            if dir.contains('$') || !dir.starts_with('/') {
                if !dir.is_empty() {
                    self.incomplete = true;
                }
            } else {
                self.dirs.push(dir.into());
            }
        }
    }

    /// Find `name` the way the loader would, returning the path of the
    /// first library with the same identity as the program.
    fn find(&self, name: &str, ident: Ident) -> Option<PathBuf> {
        let matches = |path: &Path| {
            open_regular(path)
                .ok()
                .and_then(|file| elf::read_ident(&file).ok().flatten())
                == Some(ident)
        };

        let in_dirs = |dirs: &[PathBuf]| {
            dirs.iter()
                .map(|dir| dir.join(name))
                .find(|path| matches(path))
        };

        in_dirs(&self.dirs)
            .or_else(|| {
                self.cache?
                    .iter()
                    .filter(|(soname, _)| soname == name)
                    .map(|(_, path)| path)
                    .find(|path| matches(path))
                    .cloned()
            })
            .or_else(|| in_dirs(&self.defaults))
    }
}

/// musl's loader provides these itself: `libc.so`, `libpthread.so.0`, ...
fn is_musl_builtin(name: &str) -> bool {
    name.strip_prefix("lib")
        .and_then(|rest| rest.split_once('.'))
        .is_some_and(|(lib, _)| {
            matches!(lib, "c" | "pthread" | "rt" | "m" | "dl" | "util" | "xnet")
        })
}

fn check_libraries(
    elf: &Elf,
    interp: &str,
    ld_cache: Option<&[(String, PathBuf)]>,
) -> Result<(), Incompatible> {
    let Some(flavor) = flavor(interp) else {
        // bionic, uClibc, ...: only the loader was checked.
        return Ok(());
    };
    let path = SearchPath::new(elf, interp, &flavor, ld_cache);

    // Libraries for another architecture, e.g. run through qemu-user,
    // may live in a sysroot only the emulator knows about.
    let native = host_ident() == Some(elf.ident);
    let can_tell_missing = native && !path.incomplete;

    let find = |name: &str| -> Option<PathBuf> {
        if name.contains('/') {
            return Path::new(name).is_absolute().then(|| name.into());
        }
        if file_name(interp) == name {
            return Some(interp.into());
        }
        path.find(name, elf.ident)
    };

    let mut found = Vec::with_capacity(elf.needed.len());
    for name in &elf.needed {
        if matches!(flavor, Flavor::Musl(_)) && is_musl_builtin(name) {
            continue;
        }
        match find(name) {
            Some(lib) => found.push((name.as_str(), lib)),
            None if can_tell_missing => {
                return Err(Incompatible::Library { name: name.clone() });
            }
            None => {
                #[cfg(feature = "tracing")]
                debug!("compat: {name} not found, but the search path is incomplete");
            }
        }
    }

    // musl doesn't implement symbol versioning.
    if matches!(flavor, Flavor::Musl(_)) {
        return Ok(());
    }

    for verneed in &elf.verneed {
        let Some((_, lib)) = found.iter().find(|(name, _)| *name == verneed.file) else {
            continue;
        };
        let Some(provided) = open_regular(lib)
            .ok()
            .and_then(|file| elf::parse(&file).ok().flatten())
            .map(|lib| lib.verdef)
        else {
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
        Host::load().await.check(path).await
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
        assert!(
            matches!(flavor("/lib/ld-musl-x86_64.so.1"), Some(Flavor::Musl(arch)) if arch == "x86_64")
        );
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

        let mut exe = fs::read(env::current_exe().unwrap()).unwrap();
        let mut replaced = 0;
        let mut i = 0;
        while let Some(pos) = exe[i..].windows(from.len()).position(|w| w == from) {
            exe[i + pos..i + pos + from.len()].copy_from_slice(&to);
            i += pos + from.len();
            replaced += 1;
        }
        assert!(replaced > 0);

        let path = dir.join("patched");
        fs::write(&path, exe).unwrap();
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
        if Host::load().await.ld_cache.is_none() {
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
