//! What the check needs from an ELF file, read with goblin: its identity,
//! `PT_INTERP`, and its GNU symbol version requirements and definitions.

use std::{fs::File, io, path::Path};

use goblin::{
    elf::{
        Elf as GoblinElf,
        header::{EI_CLASS, EI_DATA, ELFCLASS32, ELFCLASS64, ELFDATA2LSB, ELFDATA2MSB},
        symver::{VER_FLG_BASE, VER_FLG_WEAK},
    },
    options::ParseOptions,
};
use memmap2::Mmap;

use crate::probe::elf::{Class, Endian};

/// The ELF identity of a file: the loader only maps objects whose
/// identity matches the program's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Ident {
    pub(crate) class: Class,
    pub(crate) endian: Endian,
    pub(crate) machine: u16,
}

/// Versions required from one library, as named in `DT_NEEDED`: each
/// version name, and whether it is weak.
#[derive(Debug)]
pub(crate) struct Verneed {
    pub(crate) file: String,
    pub(crate) versions: Vec<(String, bool)>,
}

#[derive(Debug)]
pub(crate) struct Elf {
    pub(crate) ident: Ident,
    pub(crate) interp: Option<String>,
    /// `DT_RPATH` and `DT_RUNPATH` entries.
    pub(crate) rpaths: Vec<String>,
    pub(crate) verneed: Vec<Verneed>,
    /// Version names this object defines, without the base (soname) one.
    pub(crate) verdef: Vec<String>,
}

fn ident(elf: &GoblinElf<'_>) -> Option<Ident> {
    let e_ident = &elf.header.e_ident;
    Some(Ident {
        class: match e_ident[EI_CLASS] {
            ELFCLASS32 => Class::Elf32,
            ELFCLASS64 => Class::Elf64,
            _ => return None,
        },
        endian: match e_ident[EI_DATA] {
            ELFDATA2LSB => Endian::Little,
            ELFDATA2MSB => Endian::Big,
            _ => return None,
        },
        machine: elf.header.e_machine,
    })
}

/// Map `path` if it is a regular file: opening a FIFO, which an archive
/// may link to, would block.
fn map(path: &Path) -> io::Result<Mmap> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    // SAFETY: the mapping is read-only and only read from. A file
    // modified while mapped may read inconsistently, which goblin's
    // bounds-checked parsing tolerates; truncation is the caller's
    // risk, as with every other reader of these files (lddtree maps
    // them too).
    unsafe { Mmap::map(&file) }
}

/// Parse the file at `path`, or return `None` if it is not an ELF file.
pub(crate) fn parse(path: &Path) -> io::Result<Option<Elf>> {
    let bytes = map(path)?;
    // Recover from what the check doesn't need being malformed, e.g. a
    // symbol table goblin sizes wrongly: a malformed part we need reads
    // as empty, which can only make the check pass.
    let Ok(elf) = GoblinElf::parse_with_opts(&bytes, &ParseOptions::permissive()) else {
        return Ok(None);
    };
    let Some(ident) = ident(&elf) else {
        return Ok(None);
    };
    let name = |off: usize| elf.dynstrtab.get_at(off).map(str::to_owned);

    let verneed = elf
        .verneed
        .iter()
        .flat_map(|section| section.iter())
        .filter_map(|need| {
            Some(Verneed {
                file: name(need.vn_file)?,
                versions: need
                    .iter()
                    .filter_map(|aux| {
                        Some((name(aux.vna_name)?, aux.vna_flags & VER_FLG_WEAK != 0))
                    })
                    .collect(),
            })
        })
        .collect();

    let verdef = elf
        .verdef
        .iter()
        .flat_map(|section| section.iter())
        .filter(|def| def.vd_flags & VER_FLG_BASE == 0)
        // The first Verdaux names the version, the others its parents.
        .filter_map(|def| name(def.iter().next()?.vda_name))
        .collect();

    Ok(Some(Elf {
        ident,
        // The loader reads the path up to its NUL; goblin keeps any
        // padding after it.
        interp: elf
            .interpreter
            .map(|interp| interp.split('\0').next().unwrap_or_default().to_owned()),
        rpaths: elf
            .rpaths
            .iter()
            .chain(&elf.runpaths)
            .map(|path| (*path).to_owned())
            .collect(),
        verneed,
        verdef,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_elf() {
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        assert!(parse(path).unwrap().is_none());
    }

    /// goblin reads every synthesized probe back as the spec it was made
    /// from.
    #[test]
    fn reads_synthesized_probe() {
        for probe in crate::probe::probes() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("probe");
            std::fs::write(&path, probe.synthesize()).unwrap();
            let elf = parse(&path).unwrap().unwrap();
            assert_eq!(elf.ident.class, probe.spec.class);
            assert_eq!(elf.ident.endian, probe.spec.endian);
            assert_eq!(elf.ident.machine, probe.spec.e_machine);
            assert_eq!(elf.interp.as_deref(), Some(probe.spec.interp));
            assert!(elf.verneed.is_empty());

            std::fs::write(&path, probe.synthesize_static()).unwrap();
            let elf = parse(&path).unwrap().unwrap();
            assert_eq!(elf.interp, None);
        }
    }

    /// This test binary is a dynamically linked program on gnu hosts:
    /// its libc requirements must be readable, and the host's libc must
    /// define what it requires.
    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn reads_own_executable() {
        let elf = parse(&std::env::current_exe().unwrap()).unwrap().unwrap();
        let libc = elf.verneed.iter().find(|v| v.file == "libc.so.6").unwrap();
        assert!(libc.versions.iter().any(|(v, _)| v.starts_with("GLIBC_2.")));

        // Find the libc next to the loader, as glibc installs them.
        let interp = std::fs::canonicalize(elf.interp.unwrap()).unwrap();
        let libc_path = interp.parent().unwrap().join("libc.so.6");
        if libc_path.exists() {
            let libc = parse(&libc_path).unwrap().unwrap();
            assert!(
                libc.verdef.iter().any(|v| v.starts_with("GLIBC_2.")),
                "{:?}",
                libc.verdef
            );
        }
    }
}
