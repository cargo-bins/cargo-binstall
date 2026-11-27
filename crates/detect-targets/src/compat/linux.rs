use std::{fs::File, path::Path};

use goblin::elf::Elf;
use memmap2::Mmap;
use tokio::task::spawn_blocking;

use super::Incompatible;

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
    let Some((Some(interp), needed)) = parse(path, |elf| (interp(elf), needed_glibc(elf))) else {
        return Ok(());
    };
    if !Path::new(&interp).exists() {
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
            needed: display(&needed),
            available: display(&available),
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

/// The version of `GLIBC_2.39`: `[2, 39]`.
fn glibc_version(name: &str) -> Option<Vec<u32>> {
    let version = name.strip_prefix("GLIBC_")?;
    version.split('.').map(|n| n.parse().ok()).collect()
}

/// The newest glibc version `elf` requires, if any.
fn needed_glibc(elf: &Elf<'_>) -> Option<Vec<u32>> {
    let required = elf.verneed.iter().flat_map(|section| section.iter());
    required
        .flat_map(|need| need.iter().collect::<Vec<_>>())
        // A weak version is only required if available.
        .filter(|aux| aux.vna_flags & goblin::elf::symver::VER_FLG_WEAK == 0)
        .filter_map(|aux| glibc_version(elf.dynstrtab.get_at(aux.vna_name)?))
        .max()
}

/// The newest glibc version the library `elf` defines, if any.
fn available_glibc(elf: &Elf<'_>) -> Option<Vec<u32>> {
    elf.verdef
        .iter()
        .flat_map(|section| section.iter())
        .filter_map(|def| glibc_version(elf.dynstrtab.get_at(def.iter().next()?.vda_name)?))
        .max()
}

fn display(version: &[u32]) -> String {
    let parts: Vec<_> = version.iter().map(u32::to_string).collect();
    parts.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(glibc_version("GLIBC_2.39"), Some(vec![2, 39]));
        assert_eq!(glibc_version("GLIBC_2.2.5"), Some(vec![2, 2, 5]));
        assert_eq!(glibc_version("GLIBC_PRIVATE"), None);
        assert_eq!(glibc_version("GLIBCXX_3.4"), None);
        assert!(glibc_version("GLIBC_2.9") < glibc_version("GLIBC_2.10"));
        assert_eq!(display(&[2, 39]), "2.39");
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
