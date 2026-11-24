//! Synthesis of minimal dynamically-linked ELF executables used as
//! loader probes.
//!
//! The synthesized binary consists of an ELF header, five program
//! headers (`PT_PHDR`, `PT_LOAD`, `PT_INTERP`, `PT_DYNAMIC`,
//! `PT_GNU_STACK`), the interpreter path string, an empty-but-valid
//! dynamic section (empty `DT_HASH`, one null `DT_SYMTAB` entry, a
//! one-byte `DT_STRTAB` — glibc's loader dereferences these for the
//! main program without checking they exist) and a tiny
//! `exit_group(0)` stub as the entry point. There are no section
//! headers, no relocations and no `DT_NEEDED` entries, so a clean
//! `exit(0)` proves exactly one thing: the kernel found and
//! successfully ran the dynamic loader at the embedded `PT_INTERP`
//! path.
//!
//! The binary is emitted as `ET_DYN` (PIE-style) since every loader
//! accepts that, including Android's bionic linker which rejects
//! `ET_EXEC`. The stub contains no absolute addresses, so it needs no
//! relocations regardless of the load base.

/// ELF file class, `e_ident[EI_CLASS]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Class {
    Elf32 = 1,
    Elf64 = 2,
}

impl Class {
    /// Size of `Elf{32,64}_Ehdr`.
    pub(crate) const fn ehsize(self) -> usize {
        match self {
            Class::Elf32 => 52,
            Class::Elf64 => 64,
        }
    }

    /// Size of `Elf{32,64}_Phdr`.
    pub(crate) const fn phentsize(self) -> usize {
        match self {
            Class::Elf32 => 32,
            Class::Elf64 => 56,
        }
    }

    /// Size of `Elf{32,64}_Dyn`.
    pub(crate) const fn dynent(self) -> usize {
        match self {
            Class::Elf32 => 8,
            Class::Elf64 => 16,
        }
    }

    /// Size of `Elf{32,64}_Sym`.
    const fn syment(self) -> usize {
        match self {
            Class::Elf32 => 16,
            Class::Elf64 => 24,
        }
    }
}

/// ELF data encoding, `e_ident[EI_DATA]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Endian {
    Little = 1,
    Big = 2,
}

/// Everything needed to synthesize one probe executable.
pub(crate) struct ElfSpec {
    pub(crate) class: Class,
    pub(crate) endian: Endian,
    pub(crate) e_machine: u16,
    pub(crate) e_flags: u32,
    /// Absolute path of the dynamic loader to put in `PT_INTERP`.
    pub(crate) interp: &'static str,
    /// Machine code performing `exit_group(0)`, used as the entry point.
    pub(crate) stub: &'static [u8],
}

const EV_CURRENT: u8 = 1;
const ELFOSABI_NONE: u8 = 0;

const ET_DYN: u16 = 3;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;
const PT_GNU_STACK: u32 = 0x6474_e551;

const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

const DT_HASH: u64 = 4;
const DT_STRTAB: u64 = 5;
const DT_SYMTAB: u64 = 6;
const DT_STRSZ: u64 = 10;
const DT_SYMENT: u64 = 11;

/// Output buffer that encodes fields for one ELF class and encoding.
struct Out {
    buf: Vec<u8>,
    class: Class,
    endian: Endian,
}

impl Out {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn u16(&mut self, v: u16) {
        let b = match self.endian {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        };
        self.buf.extend_from_slice(&b);
    }

    fn u32(&mut self, v: u32) {
        let b = match self.endian {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        };
        self.buf.extend_from_slice(&b);
    }

    fn u64(&mut self, v: u64) {
        let b = match self.endian {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        };
        self.buf.extend_from_slice(&b);
    }

    /// Write a native-word-sized field (`Elf32_Addr`/`Elf64_Addr` etc).
    fn word(&mut self, v: u64) {
        match self.class {
            Class::Elf32 => self.u32(v as u32),
            Class::Elf64 => self.u64(v),
        }
    }

    fn pad_to(&mut self, off: usize) {
        debug_assert!(self.buf.len() <= off);
        self.buf.resize(off, 0);
    }
}

/// The parts of `Elf{32,64}_Ehdr` that vary between probes. The others are
/// fixed: no section headers, `EV_CURRENT`, `ELFOSABI_NONE`.
struct Ehdr {
    e_type: u16,
    e_machine: u16,
    e_entry: u64,
    e_phoff: u64,
    e_flags: u32,
    e_phnum: u16,
}

impl Ehdr {
    fn write(&self, out: &mut Out) {
        let start = out.buf.len();

        // e_ident
        out.buf.extend_from_slice(b"\x7fELF");
        out.u8(out.class as u8);
        out.u8(out.endian as u8);
        out.u8(EV_CURRENT);
        out.u8(ELFOSABI_NONE);
        out.u8(0); // EI_ABIVERSION
        out.buf.extend_from_slice(&[0; 7]); // EI_PAD

        out.u16(self.e_type);
        out.u16(self.e_machine);
        out.u32(EV_CURRENT.into()); // e_version
        out.word(self.e_entry);
        out.word(self.e_phoff);
        out.word(0); // e_shoff
        out.u32(self.e_flags);
        out.u16(out.class.ehsize() as u16);
        out.u16(out.class.phentsize() as u16);
        out.u16(self.e_phnum);
        out.u16(0); // e_shentsize
        out.u16(0); // e_shnum
        out.u16(0); // e_shstrndx

        debug_assert_eq!(out.buf.len() - start, out.class.ehsize());
    }
}

/// `Elf{32,64}_Phdr` of a segment whose virtual address equals its file
/// offset, and whose size in memory equals its size in the file.
struct Phdr {
    p_type: u32,
    p_flags: u32,
    offset: usize,
    size: usize,
    p_align: u64,
}

impl Phdr {
    fn write(&self, out: &mut Out) {
        let start = out.buf.len();

        out.u32(self.p_type);
        // `p_flags` comes second in Elf64_Phdr, and second to last in
        // Elf32_Phdr.
        if out.class == Class::Elf64 {
            out.u32(self.p_flags);
        }
        out.word(self.offset as u64); // p_offset
        out.word(self.offset as u64); // p_vaddr
        out.word(self.offset as u64); // p_paddr
        out.word(self.size as u64); // p_filesz
        out.word(self.size as u64); // p_memsz
        if out.class == Class::Elf32 {
            out.u32(self.p_flags);
        }
        out.word(self.p_align);

        debug_assert_eq!(out.buf.len() - start, out.class.phentsize());
    }
}

/// `Elf{32,64}_Dyn`.
struct Dyn {
    d_tag: u64,
    d_val: u64,
}

impl Dyn {
    fn write(&self, out: &mut Out) {
        out.word(self.d_tag);
        out.word(self.d_val);
    }
}

/// Synthesize the probe executable described by `spec`.
///
/// With `with_interp` set, the executable carries a `PT_INTERP` and
/// running it exercises the dynamic loader at `spec.interp`. Without
/// it, the executable is static: the kernel jumps straight to the
/// stub, so a clean exit only proves the kernel can execute this
/// architecture (natively, via compat mode, or through a binfmt
/// handler).
pub(crate) fn synthesize(spec: &ElfSpec, with_interp: bool) -> Vec<u8> {
    let class = spec.class;
    // PT_PHDR, PT_LOAD, [PT_INTERP,] PT_DYNAMIC, PT_GNU_STACK
    //
    // PT_PHDR is required: it is how the dynamic loader computes the
    // load base of an ET_DYN main program (l_addr = AT_PHDR -
    // PT_PHDR.p_vaddr); without it glibc assumes a load base of 0 and
    // crashes dereferencing unrelocated p_vaddr values.
    let phnum = if with_interp { 5usize } else { 4 };

    // File layout. Since this is ET_DYN, virtual addresses equal file
    // offsets (single PT_LOAD mapping the whole file at base 0).
    let phoff = class.ehsize();
    let interp_off = phoff + phnum * class.phentsize();
    let interp_size = if with_interp {
        spec.interp.len() + 1 // including NUL
    } else {
        0
    };
    // Empty DT_HASH: nbucket = 1, nchain = 1, bucket[0] = 0, chain[0] = 0.
    let hash_off = (interp_off + interp_size).next_multiple_of(8);
    let hash_size = 4 * 4;
    // DT_SYMTAB with only the mandatory null symbol.
    let symtab_off = hash_off + hash_size;
    // DT_STRTAB with only the initial NUL.
    let strtab_off = symtab_off + class.syment();
    let dyn_off = (strtab_off + 1).next_multiple_of(8);
    let dynamic = [
        Dyn {
            d_tag: DT_HASH,
            d_val: hash_off as u64,
        },
        Dyn {
            d_tag: DT_STRTAB,
            d_val: strtab_off as u64,
        },
        Dyn {
            d_tag: DT_SYMTAB,
            d_val: symtab_off as u64,
        },
        Dyn {
            d_tag: DT_STRSZ,
            d_val: 1,
        },
        Dyn {
            d_tag: DT_SYMENT,
            d_val: class.syment() as u64,
        },
    ];
    // ... followed by the terminating DT_NULL
    let dyn_size = (dynamic.len() + 1) * class.dynent();
    let stub_off = (dyn_off + dyn_size).next_multiple_of(16);
    let total = stub_off + spec.stub.len();

    let mut out = Out {
        buf: Vec::with_capacity(total),
        class,
        endian: spec.endian,
    };

    Ehdr {
        e_type: ET_DYN,
        e_machine: spec.e_machine,
        e_entry: stub_off as u64,
        e_phoff: phoff as u64,
        e_flags: spec.e_flags,
        e_phnum: phnum as u16,
    }
    .write(&mut out);

    let phdrs = [
        Some(Phdr {
            p_type: PT_PHDR,
            p_flags: PF_R,
            offset: phoff,
            size: phnum * class.phentsize(),
            p_align: 8,
        }),
        Some(Phdr {
            p_type: PT_LOAD,
            p_flags: PF_R | PF_X,
            offset: 0,
            size: total,
            p_align: 0x1000,
        }),
        with_interp.then_some(Phdr {
            p_type: PT_INTERP,
            p_flags: PF_R,
            offset: interp_off,
            size: interp_size,
            p_align: 1,
        }),
        Some(Phdr {
            p_type: PT_DYNAMIC,
            p_flags: PF_R,
            offset: dyn_off,
            size: dyn_size,
            p_align: 8,
        }),
        Some(Phdr {
            p_type: PT_GNU_STACK,
            p_flags: PF_R | PF_W,
            offset: 0,
            size: 0,
            p_align: 0x10,
        }),
    ];
    for phdr in phdrs.iter().flatten() {
        phdr.write(&mut out);
    }
    debug_assert_eq!(out.buf.len(), interp_off);

    if with_interp {
        out.buf.extend_from_slice(spec.interp.as_bytes());
        out.u8(0);
    }

    // Hash table: pad_to gets us there, then nbucket = nchain = 1
    // followed by an all-zero bucket and chain (zero-filled by pad_to
    // below, along with the null symbol and the one-NUL string table).
    out.pad_to(hash_off);
    out.u32(1);
    out.u32(1);

    out.pad_to(dyn_off);
    for entry in &dynamic {
        entry.write(&mut out);
    }
    // pad_to below writes the terminating all-zero DT_NULL entry.

    out.pad_to(stub_off);
    out.buf.extend_from_slice(spec.stub);

    debug_assert_eq!(out.buf.len(), total);
    out.buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn x86_64_gnu_spec() -> ElfSpec {
        ElfSpec {
            class: Class::Elf64,
            endian: Endian::Little,
            e_machine: 62, // EM_X86_64
            e_flags: 0,
            interp: "/lib64/ld-linux-x86-64.so.2",
            stub: &[0xb8, 0xe7, 0, 0, 0, 0x31, 0xff, 0x0f, 0x05],
        }
    }

    #[test]
    fn elf64_layout() {
        let spec = x86_64_gnu_spec();
        let elf = synthesize(&spec, true);

        // ELF magic, class, endian
        assert_eq!(&elf[..6], &[0x7f, b'E', b'L', b'F', 2, 1]);
        // e_type = ET_DYN, e_machine = EM_X86_64
        assert_eq!(&elf[16..20], &[3, 0, 62, 0]);
        // e_phoff = 64
        assert_eq!(&elf[32..40], 64u64.to_le_bytes());
        // e_phnum = 5
        assert_eq!(&elf[56..58], 5u16.to_le_bytes());

        // PT_INTERP segment contains the loader path with NUL
        let interp_off = 64 + 5 * 56;
        assert_eq!(
            &elf[interp_off..interp_off + spec.interp.len() + 1],
            b"/lib64/ld-linux-x86-64.so.2\0",
        );

        // entry points at the stub, which sits at the end of the file
        let entry = u64::from_le_bytes(elf[24..32].try_into().unwrap()) as usize;
        assert_eq!(&elf[entry..], spec.stub);
        // and is 16-byte aligned
        assert_eq!(entry % 16, 0);
    }

    #[test]
    fn elf32_layout() {
        let spec = ElfSpec {
            class: Class::Elf32,
            endian: Endian::Little,
            e_machine: 3, // EM_386
            e_flags: 0,
            interp: "/lib/ld-linux.so.2",
            stub: &[0x31, 0xdb, 0xb8, 0xfc, 0, 0, 0, 0xcd, 0x80],
        };
        let elf = synthesize(&spec, true);

        assert_eq!(&elf[..6], &[0x7f, b'E', b'L', b'F', 1, 1]);
        // e_phoff = 52
        assert_eq!(&elf[28..32], 52u32.to_le_bytes());

        let interp_off = 52 + 5 * 32;
        assert_eq!(
            &elf[interp_off..interp_off + spec.interp.len() + 1],
            b"/lib/ld-linux.so.2\0",
        );

        let entry = u32::from_le_bytes(elf[24..28].try_into().unwrap()) as usize;
        assert_eq!(&elf[entry..], spec.stub);
    }

    #[test]
    fn big_endian_fields() {
        let spec = ElfSpec {
            class: Class::Elf64,
            endian: Endian::Big,
            e_machine: 22, // EM_S390
            e_flags: 0,
            interp: "/lib/ld64.so.1",
            stub: &[0xa7, 0x29, 0, 0, 0x0a, 0xf8],
        };
        let elf = synthesize(&spec, true);

        assert_eq!(&elf[..6], &[0x7f, b'E', b'L', b'F', 2, 2]);
        assert_eq!(&elf[16..20], &[0, 3, 0, 22]);
        assert_eq!(&elf[32..40], 64u64.to_be_bytes());
    }

    #[test]
    fn static_layout() {
        let spec = x86_64_gnu_spec();
        let elf = synthesize(&spec, false);

        // e_phnum = 4: no PT_INTERP
        assert_eq!(&elf[56..58], 4u16.to_le_bytes());
        // and no interpreter path anywhere in the file
        assert!(
            !elf.windows(spec.interp.len())
                .any(|w| w == spec.interp.as_bytes())
        );

        // entry still points at the stub at the end of the file
        let entry = u64::from_le_bytes(elf[24..32].try_into().unwrap()) as usize;
        assert_eq!(&elf[entry..], spec.stub);
    }
}
