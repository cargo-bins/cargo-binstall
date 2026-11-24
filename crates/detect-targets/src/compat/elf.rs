//! A minimal reader for the parts of an ELF file the dynamic loader looks
//! at before running a program: the header, `PT_INTERP`, and the dynamic
//! section (`DT_NEEDED`, `DT_RPATH`/`DT_RUNPATH`, and the GNU symbol
//! version requirements and definitions).
//!
//! It reads through the program headers and the dynamic section, the
//! same way the loader does, so it doesn't need section headers and works
//! on `sstrip`ped files. Everything read is bounds-checked and size-capped:
//! the files come from untrusted archives.

use std::{fs::File, io, os::unix::fs::FileExt};

use crate::probe::elf::{Class, Endian};

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;

const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;
const DT_STRSZ: u64 = 10;
const DT_RPATH: u64 = 15;
const DT_RUNPATH: u64 = 29;
const DT_VERDEF: u64 = 0x6fff_fffc;
const DT_VERDEFNUM: u64 = 0x6fff_fffd;
const DT_VERNEED: u64 = 0x6fff_fffe;
const DT_VERNEEDNUM: u64 = 0x6fff_ffff;

const VER_FLG_BASE: u16 = 0x1;
const VER_FLG_WEAK: u16 = 0x2;

/// `e_phnum` value meaning the real count is stored elsewhere.
const PN_XNUM: u16 = 0xffff;

/// Caps on how much is read, far above what real files use.
const MAX_INTERP: u64 = 4096;
const MAX_DYNAMIC: u64 = 1 << 20;
const MAX_STRTAB: u64 = 64 << 20;
const MAX_VERSION_ENTRIES: u64 = 1 << 16;

/// The ELF identity of a file: the loader only maps objects whose
/// identity matches the program's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Ident {
    pub(crate) class: Class,
    pub(crate) endian: Endian,
    pub(crate) machine: u16,
}

/// One `Elf_Verneed` entry: versions required from one library.
#[derive(Debug, Default)]
pub(crate) struct Verneed {
    /// The library, as named in `DT_NEEDED`.
    pub(crate) file: String,
    /// The required version names and whether each is weak.
    pub(crate) versions: Vec<(String, bool)>,
}

#[derive(Debug)]
pub(crate) struct Elf {
    pub(crate) ident: Ident,
    pub(crate) interp: Option<String>,
    pub(crate) needed: Vec<String>,
    pub(crate) rpath: Option<String>,
    pub(crate) runpath: Option<String>,
    pub(crate) verneed: Vec<Verneed>,
    /// Version names this object defines, without the base (soname) one.
    pub(crate) verdef: Vec<String>,
}

/// Reads fields of one class and encoding from a file.
struct Reader<'a> {
    file: &'a File,
    class: Class,
    endian: Endian,
}

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl Reader<'_> {
    fn bytes(&self, off: u64, len: u64) -> io::Result<Vec<u8>> {
        let len = usize::try_from(len).map_err(|_| invalid("ELF field too large"))?;
        let mut buf = vec![0; len];
        self.file.read_exact_at(&mut buf, off)?;
        Ok(buf)
    }

    fn u16(&self, b: &[u8], at: usize) -> io::Result<u16> {
        let b: [u8; 2] = b
            .get(at..at + 2)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| invalid("truncated ELF field"))?;
        Ok(match self.endian {
            Endian::Little => u16::from_le_bytes(b),
            Endian::Big => u16::from_be_bytes(b),
        })
    }

    fn u32(&self, b: &[u8], at: usize) -> io::Result<u32> {
        let b: [u8; 4] = b
            .get(at..at + 4)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| invalid("truncated ELF field"))?;
        Ok(match self.endian {
            Endian::Little => u32::from_le_bytes(b),
            Endian::Big => u32::from_be_bytes(b),
        })
    }

    fn u64(&self, b: &[u8], at: usize) -> io::Result<u64> {
        let b: [u8; 8] = b
            .get(at..at + 8)
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| invalid("truncated ELF field"))?;
        Ok(match self.endian {
            Endian::Little => u64::from_le_bytes(b),
            Endian::Big => u64::from_be_bytes(b),
        })
    }

    /// A native-word-sized field (`Elf32_Addr`/`Elf64_Addr` etc).
    fn word(&self, b: &[u8], at: usize) -> io::Result<u64> {
        match self.class {
            Class::Elf32 => self.u32(b, at).map(u64::from),
            Class::Elf64 => self.u64(b, at),
        }
    }
}

/// A `PT_LOAD` segment, to turn the virtual addresses in the dynamic
/// section into file offsets.
struct Load {
    vaddr: u64,
    offset: u64,
    filesz: u64,
}

fn vaddr_to_offset(loads: &[Load], vaddr: u64) -> io::Result<u64> {
    loads
        .iter()
        .find(|l| vaddr >= l.vaddr && vaddr - l.vaddr < l.filesz)
        .map(|l| l.offset + (vaddr - l.vaddr))
        .ok_or_else(|| invalid("ELF address outside of any PT_LOAD"))
}

/// The NUL-terminated string at `off` in a string table.
fn string_at(strtab: &[u8], off: u64) -> io::Result<String> {
    let rest = usize::try_from(off)
        .ok()
        .and_then(|off| strtab.get(off..))
        .ok_or_else(|| invalid("ELF string offset out of bounds"))?;
    let end = rest
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| invalid("unterminated ELF string"))?;
    String::from_utf8(rest[..end].to_vec()).map_err(|_| invalid("ELF string is not UTF-8"))
}

/// Read just the ELF identity, or `None` if `file` is not an ELF file.
pub(crate) fn read_ident(file: &File) -> io::Result<Option<Ident>> {
    let mut ehdr = [0; 20];
    match file.read_exact_at(&mut ehdr, 0) {
        Ok(()) => (),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &ehdr[..4] != b"\x7fELF" {
        return Ok(None);
    }
    let class = match ehdr[4] {
        1 => Class::Elf32,
        2 => Class::Elf64,
        _ => return Ok(None),
    };
    let endian = match ehdr[5] {
        1 => Endian::Little,
        2 => Endian::Big,
        _ => return Ok(None),
    };
    let r = Reader {
        file,
        class,
        endian,
    };
    Ok(Some(Ident {
        class,
        endian,
        machine: r.u16(&ehdr, 18)?,
    }))
}

/// Parse `file`, or return `None` if it is not an ELF file.
pub(crate) fn parse(file: &File) -> io::Result<Option<Elf>> {
    let Some(ident) = read_ident(file)? else {
        return Ok(None);
    };
    let r = Reader {
        file,
        class: ident.class,
        endian: ident.endian,
    };

    // Elf{32,64}_Ehdr
    let ehdr = r.bytes(0, ident.class.ehsize() as u64)?;
    let (phoff, phentsize, phnum) = match ident.class {
        Class::Elf32 => (
            r.u32(&ehdr, 28)?.into(),
            r.u16(&ehdr, 42)?,
            r.u16(&ehdr, 44)?,
        ),
        Class::Elf64 => (r.u64(&ehdr, 32)?, r.u16(&ehdr, 54)?, r.u16(&ehdr, 56)?),
    };
    if phnum == PN_XNUM {
        return Err(invalid("ELF with PN_XNUM program headers"));
    }
    if usize::from(phentsize) < ident.class.phentsize() {
        return Err(invalid("ELF program header entries too small"));
    }

    // Elf{32,64}_Phdr
    let phdrs = r.bytes(phoff, u64::from(phentsize) * u64::from(phnum))?;
    let mut loads = Vec::new();
    let mut interp = None;
    let mut dynamic = None;
    for phdr in phdrs.chunks_exact(phentsize.into()) {
        let p_type = r.u32(phdr, 0)?;
        let (offset, vaddr, filesz) = match ident.class {
            Class::Elf32 => (r.word(phdr, 4)?, r.word(phdr, 8)?, r.word(phdr, 16)?),
            Class::Elf64 => (r.word(phdr, 8)?, r.word(phdr, 16)?, r.word(phdr, 32)?),
        };
        match p_type {
            PT_LOAD => loads.push(Load {
                vaddr,
                offset,
                filesz,
            }),
            PT_INTERP => interp = Some((offset, filesz)),
            PT_DYNAMIC => dynamic = Some((offset, filesz)),
            _ => (),
        }
    }

    let interp = match interp {
        Some((offset, size)) if size <= MAX_INTERP => {
            let bytes = r.bytes(offset, size)?;
            // The path is NUL-terminated within the segment.
            Some(string_at(&bytes, 0)?)
        }
        Some(_) => return Err(invalid("ELF PT_INTERP too large")),
        None => None,
    };

    let mut elf = Elf {
        ident,
        interp,
        needed: Vec::new(),
        rpath: None,
        runpath: None,
        verneed: Vec::new(),
        verdef: Vec::new(),
    };

    let Some((dyn_off, dyn_size)) = dynamic else {
        return Ok(Some(elf));
    };
    if dyn_size > MAX_DYNAMIC {
        return Err(invalid("ELF dynamic section too large"));
    }

    // Elf{32,64}_Dyn
    let dynent = ident.class.dynent();
    let dyns = r.bytes(dyn_off, dyn_size)?;
    let mut needed = Vec::new();
    let (mut strtab, mut strsz) = (None, None);
    let (mut rpath, mut runpath) = (None, None);
    let (mut verneed, mut verneednum) = (None, 0);
    let (mut verdef, mut verdefnum) = (None, 0);
    for entry in dyns.chunks_exact(dynent) {
        let tag = r.word(entry, 0)?;
        let val = r.word(entry, dynent / 2)?;
        match tag {
            DT_NULL => break,
            DT_NEEDED => needed.push(val),
            DT_STRTAB => strtab = Some(val),
            DT_STRSZ => strsz = Some(val),
            DT_RPATH => rpath = Some(val),
            DT_RUNPATH => runpath = Some(val),
            DT_VERNEED => verneed = Some(val),
            DT_VERNEEDNUM => verneednum = val,
            DT_VERDEF => verdef = Some(val),
            DT_VERDEFNUM => verdefnum = val,
            _ => (),
        }
    }

    let (Some(strtab), Some(strsz)) = (strtab, strsz) else {
        // Nothing in the dynamic section can be named without one.
        return Ok(Some(elf));
    };
    if strsz > MAX_STRTAB {
        return Err(invalid("ELF string table too large"));
    }
    let strtab = r.bytes(vaddr_to_offset(&loads, strtab)?, strsz)?;

    elf.needed = needed
        .into_iter()
        .map(|off| string_at(&strtab, off))
        .collect::<io::Result<_>>()?;
    elf.rpath = rpath.map(|off| string_at(&strtab, off)).transpose()?;
    elf.runpath = runpath.map(|off| string_at(&strtab, off)).transpose()?;

    if verneednum > MAX_VERSION_ENTRIES || verdefnum > MAX_VERSION_ENTRIES {
        return Err(invalid("too many ELF version entries"));
    }

    if let Some(verneed) = verneed {
        // Elf_Verneed and Elf_Vernaux are 16 bytes in both classes.
        let mut off = vaddr_to_offset(&loads, verneed)?;
        for _ in 0..verneednum {
            let vn = r.bytes(off, 16)?;
            let vn_cnt = r.u16(&vn, 2)?;
            let vn_file = r.u32(&vn, 4)?;
            let vn_aux = r.u32(&vn, 8)?;
            let vn_next = r.u32(&vn, 12)?;

            let mut entry = Verneed {
                file: string_at(&strtab, vn_file.into())?,
                versions: Vec::with_capacity(vn_cnt.into()),
            };
            let mut aux_off = off + u64::from(vn_aux);
            for _ in 0..vn_cnt {
                let vna = r.bytes(aux_off, 16)?;
                let vna_flags = r.u16(&vna, 4)?;
                let vna_name = r.u32(&vna, 8)?;
                let vna_next = r.u32(&vna, 12)?;
                entry.versions.push((
                    string_at(&strtab, vna_name.into())?,
                    vna_flags & VER_FLG_WEAK != 0,
                ));
                if vna_next == 0 {
                    break;
                }
                aux_off += u64::from(vna_next);
            }
            elf.verneed.push(entry);

            if vn_next == 0 {
                break;
            }
            off += u64::from(vn_next);
        }
    }

    if let Some(verdef) = verdef {
        // Elf_Verdef is 20 bytes and Elf_Verdaux 8 in both classes.
        let mut off = vaddr_to_offset(&loads, verdef)?;
        for _ in 0..verdefnum {
            let vd = r.bytes(off, 20)?;
            let vd_flags = r.u16(&vd, 2)?;
            let vd_cnt = r.u16(&vd, 6)?;
            let vd_aux = r.u32(&vd, 12)?;
            let vd_next = r.u32(&vd, 16)?;

            // The first Verdaux names the version; the others name its
            // parents. The base entry names the object itself.
            if vd_flags & VER_FLG_BASE == 0 && vd_cnt > 0 {
                let vda = r.bytes(off + u64::from(vd_aux), 8)?;
                elf.verdef.push(string_at(&strtab, r.u32(&vda, 0)?.into())?);
            }

            if vd_next == 0 {
                break;
            }
            off += u64::from(vd_next);
        }
    }

    Ok(Some(elf))
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;

    #[test]
    fn not_elf() {
        let file = File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        assert!(parse(&file).unwrap().is_none());
    }

    /// The probe executables are valid ELF files: reading one back
    /// must give the spec it was synthesized from.
    #[test]
    fn reads_synthesized_probe() {
        for probe in crate::probe::probes() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("probe");
            std::fs::write(&path, probe.synthesize()).unwrap();
            let elf = parse(&File::open(&path).unwrap()).unwrap().unwrap();
            assert_eq!(elf.ident.class, probe.spec.class);
            assert_eq!(elf.ident.endian, probe.spec.endian);
            assert_eq!(elf.ident.machine, probe.spec.e_machine);
            assert_eq!(elf.interp.as_deref(), Some(probe.spec.interp));
            assert!(elf.needed.is_empty());
            assert!(elf.verneed.is_empty());

            std::fs::write(&path, probe.synthesize_static()).unwrap();
            let elf = parse(&File::open(&path).unwrap()).unwrap().unwrap();
            assert_eq!(elf.interp, None);
        }
    }

    /// This test binary is a dynamically linked program on gnu hosts:
    /// its libc requirements must be readable, and the host's libc must
    /// define what it requires.
    #[test]
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn reads_own_executable() {
        use std::{env, path::Path};

        let exe = env::current_exe().unwrap();
        let elf = parse(&File::open(exe).unwrap()).unwrap().unwrap();
        assert!(elf.interp.is_some());
        assert!(elf.needed.iter().any(|n| n == "libc.so.6"), "{elf:?}");
        let libc = elf.verneed.iter().find(|v| v.file == "libc.so.6").unwrap();
        assert!(libc.versions.iter().any(|(v, _)| v.starts_with("GLIBC_2.")));

        // Find the libc next to the loader, as glibc installs them.
        let interp = std::fs::canonicalize(elf.interp.unwrap()).unwrap();
        let libc_path = interp.parent().unwrap().join("libc.so.6");
        if Path::new(&libc_path).exists() {
            let libc = parse(&File::open(libc_path).unwrap()).unwrap().unwrap();
            assert!(
                libc.verdef
                    .iter()
                    .any(|v| v == "GLIBC_2.2.5" || v == "GLIBC_2.17" || v == "GLIBC_2.0"),
                "{:?}",
                libc.verdef
            );
        }
    }
}
