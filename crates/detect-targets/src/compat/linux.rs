use std::{fs::File, path::Path, process::Stdio, time::Duration};

use goblin::elf::Elf;
use memmap2::Mmap;
use tokio::{process::Command, task::spawn_blocking, time::timeout};

use super::Incompatible;
use crate::probe;

/// Check whether the executable at `path` can run on this system, see
/// the [module docs](super).
///
/// Returns `Ok` if it can, or if that can't be determined, including when
/// `path` is not an ELF file or can't be read.
pub async fn check(path: &Path) -> Result<(), Incompatible> {
    let binary = path.to_owned();
    // `None` for a static binary: target detection already established
    // that the kernel runs it.
    let Ok(Some(Some(interp))) = spawn_blocking(move || read_interp(&binary)).await else {
        return Ok(());
    };

    if !Path::new(&interp).exists() {
        return Err(Incompatible::Loader { interp });
    }
    // Only run a loader known to be one: a standard path, holding a real
    // loader (one with no loader of its own, unlike e.g. gcompat's shim,
    // which runs the program).
    let loader = interp.clone();
    let trusted = probe::probes().iter().any(|p| p.spec.interp == interp)
        && spawn_blocking(move || read_interp(Path::new(&loader)) == Some(None))
            .await
            .unwrap_or(false);
    if !trusted {
        return Ok(());
    }

    // A path without a `/` would be looked up like a library name.
    let Ok(path) = std::path::absolute(path) else {
        return Ok(());
    };
    let output = Command::new(&interp)
        .arg("--list")
        .arg(path)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    match timeout(Duration::from_secs(10), output).await {
        Ok(Ok(output)) => parse_list(&String::from_utf8_lossy(&output.stderr))
            .or_else(|| parse_list(&String::from_utf8_lossy(&output.stdout)))
            .map_or(Ok(()), Err),
        // Can't run it (noexec, seccomp, a foreign arch without binfmt):
        // inconclusive.
        _ => Ok(()),
    }
}

/// `Some(PT_INTERP)` of the ELF file at `path`, `Some(None)` for one
/// without, `None` if it is not an ELF file or can't be read.
fn read_interp(path: &Path) -> Option<Option<String>> {
    // Archives may hold FIFOs and device nodes: opening a FIFO would
    // block, and opening a device can be an action of its own.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    // SAFETY: read-only mapping, only read by goblin's bounds-checked
    // parser.
    let bytes = unsafe { Mmap::map(&file) }.ok()?;
    let elf = Elf::parse(&bytes).ok()?;
    Some(
        elf.interpreter
            .map(|interp| interp.split('\0').next().unwrap_or_default().into()),
    )
}

/// The first problem the loader reports in its `--list` output, if any.
fn parse_list(output: &str) -> Option<Incompatible> {
    output.lines().find_map(|line| {
        // glibc: "BIN: LIB: version `GLIBC_2.39' not found (required by BIN)"
        if let Some((before, rest)) = line.split_once(": version `") {
            let (version, _) = rest.split_once("' not found")?;
            let library = before.rsplit_once(": ")?.1;
            let library = library.rsplit_once('/').map_or(library, |(_, name)| name);
            return Some(Incompatible::Version {
                library: library.into(),
                version: version.into(),
            });
        }
        let name = if let Some(rest) = line.split_once("error while loading shared libraries: ") {
            // glibc: "BIN: error while loading shared libraries: LIB: cannot open ..."
            rest.1.split_once(": ")?.0
        } else if let Some(rest) = line.strip_prefix("Error loading shared library ") {
            // musl: "Error loading shared library LIB: No such file ... (needed by BIN)"
            rest.split_once(": ")?.0
        } else {
            // glibc, for a library needed by another: "\tLIB => not found"
            line.trim().strip_suffix(" => not found")?
        };
        Some(Incompatible::Library { name: name.into() })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_output() {
        assert_eq!(
            parse_list(
                "./t: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_9.3' not found (required by ./t)\n"
            ),
            Some(Incompatible::Version {
                library: "libc.so.6".into(),
                version: "GLIBC_9.3".into()
            })
        );
        let library = |name: &str| Some(Incompatible::Library { name: name.into() });
        assert_eq!(
            parse_list(
                "./t: error while loading shared libraries: libQ.so.6: cannot open shared object file: No such file or directory\n"
            ),
            library("libQ.so.6")
        );
        assert_eq!(
            parse_list(
                "Error loading shared library libfoo.so: No such file or directory (needed by /t)\n"
            ),
            library("libfoo.so")
        );
        assert_eq!(
            parse_list("\tlibz.so.1 => /lib/libz.so.1 (0x1)\n\tlibQ.so.6 => not found\n"),
            library("libQ.so.6")
        );
        assert_eq!(
            parse_list("\tlinux-vdso.so.1 (0x1)\n\tlibc.so.6 => /lib/libc.so.6 (0x2)\n"),
            None
        );
    }

    /// This test binary runs, so it must pass the check.
    #[tokio::test]
    async fn own_executable_compatible() {
        check(&std::env::current_exe().unwrap()).await.unwrap();
    }

    /// Not an ELF file, or missing: nothing to check.
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

    /// A copy of this test binary with `from` replaced by `to`, NUL-padded
    /// to keep every offset in place.
    #[cfg(target_env = "gnu")]
    fn patched_exe(dir: &Path, from: &[u8], to: &[u8]) -> std::path::PathBuf {
        let mut to = to.to_vec();
        to.resize(from.len(), 0);
        let mut exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let mut i = 0;
        while let Some(pos) = exe[i..].windows(from.len()).position(|w| w == from) {
            exe[i + pos..][..from.len()].copy_from_slice(&to);
            i += pos + from.len();
        }
        assert!(i > 0, "nothing to patch");
        let path = dir.join("patched");
        std::fs::write(&path, exe).unwrap();
        path
    }

    /// A binary needing a newer glibc than the host's, linking to a
    /// library the host lacks, or using a missing loader is rejected.
    #[tokio::test]
    #[cfg(target_env = "gnu")]
    async fn incompatible_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let exe = patched_exe(dir.path(), b"GLIBC_2.", b"GLIBC_9.");
        assert!(
            matches!(check(&exe).await, Err(Incompatible::Version { version, .. }) if version.starts_with("GLIBC_9.")),
        );

        let exe = patched_exe(dir.path(), b"libc.so.6\0", b"libQ.so.6\0");
        assert_eq!(
            check(&exe).await,
            Err(Incompatible::Library {
                name: "libQ.so.6".into()
            })
        );

        let interp = read_interp(&std::env::current_exe().unwrap())
            .unwrap()
            .unwrap();
        let exe = patched_exe(dir.path(), interp.as_bytes(), b"/nonexistent/ld.so");
        assert_eq!(
            check(&exe).await,
            Err(Incompatible::Loader {
                interp: "/nonexistent/ld.so".into()
            })
        );
    }

    /// Targets given explicitly are not checked.
    #[tokio::test]
    #[cfg(target_env = "gnu")]
    async fn explicit_targets_not_checked() {
        let dir = tempfile::tempdir().unwrap();
        let exe = patched_exe(dir.path(), b"GLIBC_2.", b"GLIBC_9.");
        assert!(check(&exe).await.is_err());

        let targets = crate::get_desired_targets(Some(vec![crate::TARGET.into()]));
        targets.check_binary(&exe).await.unwrap();
    }
}
