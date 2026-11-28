use std::{fs::File, path::Path};

use goblin::elf::Elf;
use memmap2::Mmap;
use tokio::task::spawn_blocking;

use super::Incompatible;
use crate::{
    TARGET,
    probe::{Probe, find},
};

/// Check whether the executable at `path` can run on this system, see
/// the [module docs](super).
///
/// Returns `Ok` if it can, or if that can't be determined, including when
/// `path` is not an ELF file or can't be read.
pub async fn check(path: &Path) -> Result<(), Incompatible> {
    let path = path.to_owned();
    spawn_blocking(move || check_blocking(&path))
        .await
        .unwrap_or(Ok(()))
}

fn check_blocking(path: &Path) -> Result<(), Incompatible> {
    // A static binary has no loader: target detection already established
    // that the kernel runs it.
    let Some((machine, Some(interp), needed)) = parse(path, |elf| {
        (elf.header.e_machine, interp(elf), needed_glibc(elf))
    }) else {
        return Ok(());
    };
    // A foreign binary may run under an emulator that looks up its loader
    // and libraries in a sysroot of its own.
    if Some(machine) != find(TARGET).map(Probe::e_machine) {
        return Ok(());
    }
    if matches!(Path::new(&interp).try_exists(), Ok(false)) {
        return Err(Incompatible::Loader { interp });
    }

    // glibc installs `libc.so.6` next to its loader.
    let Some(needed) = needed else {
        return Ok(());
    };
    let Some(libc) = std::fs::canonicalize(&interp)
        .ok()
        .and_then(|loader| Some(loader.parent()?.join("libc.so.6")))
    else {
        return Ok(());
    };
    match parse(&libc, available_glibc).flatten() {
        Some(available) if available < needed => Err(Incompatible::Glibc {
            needed: display(needed),
            available: display(available),
        }),
        _ => Ok(()),
    }
}

/// Parse the ELF file at `path` with `f`, or `None` if it is not one or
/// can't be read.
fn parse<T>(path: &Path, f: impl FnOnce(&Elf<'_>) -> T) -> Option<T> {
    // Archives may hold FIFOs and device nodes: opening a FIFO would
    // block, and opening a device can be an action of its own.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = File::open(path).ok()?;
    // SAFETY: read-only mapping, only read by goblin's bounds-checked
    // parser.
    let bytes = unsafe { Mmap::map(&file) }.ok()?;
    Some(f(&Elf::parse(&bytes).ok()?))
}

fn interp(elf: &Elf<'_>) -> Option<String> {
    Some(elf.interpreter?.split('\0').next()?.to_owned())
}

/// The version of `GLIBC_2.39`: `[2, 39, 0]`. glibc versions have two or
/// three parts, e.g. `GLIBC_2.2.5`.
fn glibc_version(name: &str) -> Option<[u32; 3]> {
    let mut version = [0; 3];
    let mut parts = name.strip_prefix("GLIBC_")?.split('.');
    for (n, part) in version.iter_mut().zip(&mut parts) {
        *n = part.parse().ok()?;
    }
    parts.next().is_none().then_some(version)
}

/// The newest glibc version `elf` requires, if any.
fn needed_glibc(elf: &Elf<'_>) -> Option<[u32; 3]> {
    elf.verneed
        .iter()
        .flat_map(|section| section.iter())
        // Other glibc libraries define versions libc.so.6 doesn't, e.g.
        // libm.so.6's GLIBC_2.40 in glibc 2.40.
        .filter(|need| elf.dynstrtab.get_at(need.vn_file) == Some("libc.so.6"))
        .filter_map(|need| {
            need.iter()
                // A weak version is only required if available.
                .filter(|aux| aux.vna_flags & goblin::elf::symver::VER_FLG_WEAK == 0)
                .filter_map(|aux| glibc_version(elf.dynstrtab.get_at(aux.vna_name)?))
                .max()
        })
        .max()
}

/// The newest glibc version the library `elf` defines, if any.
fn available_glibc(elf: &Elf<'_>) -> Option<[u32; 3]> {
    elf.verdef
        .iter()
        .flat_map(|section| section.iter())
        .filter_map(|def| glibc_version(elf.dynstrtab.get_at(def.iter().next()?.vda_name)?))
        .max()
}

fn display([major, minor, patch]: [u32; 3]) -> String {
    match patch {
        0 => format!("{major}.{minor}"),
        _ => format!("{major}.{minor}.{patch}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(glibc_version("GLIBC_2.39"), Some([2, 39, 0]));
        assert_eq!(glibc_version("GLIBC_2.2.5"), Some([2, 2, 5]));
        assert_eq!(glibc_version("GLIBC_2.1.2.3"), None);
        assert_eq!(glibc_version("GLIBC_PRIVATE"), None);
        assert_eq!(glibc_version("GLIBCXX_3.4"), None);
        assert!(glibc_version("GLIBC_2.9") < glibc_version("GLIBC_2.10"));
        assert!(glibc_version("GLIBC_2.2") < glibc_version("GLIBC_2.2.5"));
        assert!(glibc_version("GLIBC_2.2.5") < glibc_version("GLIBC_2.3"));
        assert_eq!(display([2, 39, 0]), "2.39");
        assert_eq!(display([2, 2, 5]), "2.2.5");
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

    /// A binary needing a newer glibc than the host's, or using a missing
    /// loader, is rejected.
    #[tokio::test]
    #[cfg(target_env = "gnu")]
    async fn incompatible_rejected() {
        let dir = tempfile::tempdir().unwrap();

        let exe = patched_exe(dir.path(), b"GLIBC_2.", b"GLIBC_9.");
        assert!(
            matches!(check(&exe).await, Err(Incompatible::Glibc { needed, .. }) if needed.starts_with("9."))
        );

        let interp = parse(&std::env::current_exe().unwrap(), interp)
            .flatten()
            .unwrap();
        let exe = patched_exe(dir.path(), interp.as_bytes(), b"/nonexistent/ld.so");
        assert_eq!(
            check(&exe).await,
            Err(Incompatible::Loader {
                interp: "/nonexistent/ld.so".into()
            })
        );
    }

    /// A binary for another architecture is not checked.
    #[tokio::test]
    #[cfg(target_env = "gnu")]
    async fn foreign_machine_not_checked() {
        let dir = tempfile::tempdir().unwrap();
        let exe = patched_exe(dir.path(), b"GLIBC_2.", b"GLIBC_9.");
        let mut bytes = std::fs::read(&exe).unwrap();
        // `e_machine`, with a value no probe has.
        bytes[18..20].fill(0xff);
        std::fs::write(&exe, bytes).unwrap();
        check(&exe).await.unwrap();
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
