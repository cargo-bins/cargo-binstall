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

/// ELF file class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Class {
    Elf32,
    Elf64,
}

/// ELF data encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Endian {
    Little,
    Big,
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

/// Endian-aware output buffer.
struct Out {
    buf: Vec<u8>,
    big: bool,
}

impl Out {
    fn u16(&mut self, v: u16) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.buf.extend_from_slice(&b);
    }

    fn u32(&mut self, v: u32) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.buf.extend_from_slice(&b);
    }

    fn u64(&mut self, v: u64) {
        let b = if self.big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        };
        self.buf.extend_from_slice(&b);
    }

    /// Write a native-word-sized field (`Elf32_Addr`/`Elf64_Addr` etc).
    fn word(&mut self, class: Class, v: u64) {
        match class {
            Class::Elf32 => self.u32(v as u32),
            Class::Elf64 => self.u64(v),
        }
    }

    fn pad_to(&mut self, off: usize) {
        debug_assert!(self.buf.len() <= off);
        self.buf.resize(off, 0);
    }
}

fn align(v: usize, to: usize) -> usize {
    (v + to - 1) / to * to
}

/// Synthesize the probe executable described by `spec`.
pub(crate) fn synthesize(spec: &ElfSpec) -> Vec<u8> {
    let class = spec.class;
    let (ehsize, phentsize, dynent, syment) = match class {
        // sizes of Ehdr, Phdr, Dyn, Sym
        Class::Elf64 => (64usize, 56usize, 16usize, 24usize),
        Class::Elf32 => (52, 32, 8, 16),
    };
    // PT_PHDR, PT_LOAD, PT_INTERP, PT_DYNAMIC, PT_GNU_STACK
    //
    // PT_PHDR is required: it is how the dynamic loader computes the
    // load base of an ET_DYN main program (l_addr = AT_PHDR -
    // PT_PHDR.p_vaddr); without it glibc assumes a load base of 0 and
    // crashes dereferencing unrelocated p_vaddr values.
    let phnum = 5usize;

    // File layout. Since this is ET_DYN, virtual addresses equal file
    // offsets (single PT_LOAD mapping the whole file at base 0).
    let phoff = ehsize;
    let interp_off = phoff + phnum * phentsize;
    let interp_size = spec.interp.len() + 1; // including NUL
    // Empty DT_HASH: nbucket = 1, nchain = 1, bucket[0] = 0, chain[0] = 0.
    let hash_off = align(interp_off + interp_size, 8);
    let hash_size = 4 * 4;
    // DT_SYMTAB with only the mandatory null symbol.
    let symtab_off = hash_off + hash_size;
    // DT_STRTAB with only the initial NUL.
    let strtab_off = symtab_off + syment;
    let dyn_off = align(strtab_off + 1, 8);
    // DT_HASH, DT_STRTAB, DT_SYMTAB, DT_STRSZ, DT_SYMENT, DT_NULL
    let dyn_size = 6 * dynent;
    let stub_off = align(dyn_off + dyn_size, 16);
    let total = stub_off + spec.stub.len();

    let mut out = Out {
        buf: Vec::with_capacity(total),
        big: spec.endian == Endian::Big,
    };

    // ELF header
    out.buf.extend_from_slice(&[
        0x7f, b'E', b'L', b'F',
        match class {
            Class::Elf32 => 1,
            Class::Elf64 => 2,
        },
        match spec.endian {
            Endian::Little => 1,
            Endian::Big => 2,
        },
        1, // EV_CURRENT
        0, // ELFOSABI_NONE
        0, // ABI version
        0, 0, 0, 0, 0, 0, 0, // padding
    ]);
    out.u16(ET_DYN);
    out.u16(spec.e_machine);
    out.u32(1); // e_version
    out.word(class, stub_off as u64); // e_entry
    out.word(class, phoff as u64); // e_phoff
    out.word(class, 0); // e_shoff
    out.u32(spec.e_flags);
    out.u16(ehsize as u16);
    out.u16(phentsize as u16);
    out.u16(phnum as u16);
    out.u16(0); // e_shentsize
    out.u16(0); // e_shnum
    out.u16(0); // e_shstrndx
    debug_assert_eq!(out.buf.len(), ehsize);

    let phdr = |out: &mut Out, p_type: u32, p_flags: u32, off: usize, size: usize, p_align: u64| {
        out.u32(p_type);
        if class == Class::Elf64 {
            out.u32(p_flags);
        }
        out.word(class, off as u64); // p_offset
        out.word(class, off as u64); // p_vaddr
        out.word(class, off as u64); // p_paddr
        out.word(class, size as u64); // p_filesz
        out.word(class, size as u64); // p_memsz
        if class == Class::Elf32 {
            out.u32(p_flags);
        }
        out.word(class, p_align);
    };

    phdr(&mut out, PT_PHDR, PF_R, phoff, phnum * phentsize, 8);
    phdr(&mut out, PT_LOAD, PF_R | PF_X, 0, total, 0x1000);
    phdr(&mut out, PT_INTERP, PF_R, interp_off, interp_size, 1);
    phdr(&mut out, PT_DYNAMIC, PF_R, dyn_off, dyn_size, 8);
    phdr(&mut out, PT_GNU_STACK, PF_R | PF_W, 0, 0, 0x10);
    debug_assert_eq!(out.buf.len(), interp_off);

    out.buf.extend_from_slice(spec.interp.as_bytes());
    out.buf.push(0);

    // Hash table: pad_to gets us there, then nbucket = nchain = 1
    // followed by an all-zero bucket and chain (zero-filled by pad_to
    // below, along with the null symbol and the one-NUL string table).
    out.pad_to(hash_off);
    out.u32(1);
    out.u32(1);

    out.pad_to(dyn_off);
    let dyn_entry = |out: &mut Out, tag: u64, val: u64| {
        out.word(class, tag);
        out.word(class, val);
    };
    dyn_entry(&mut out, DT_HASH, hash_off as u64);
    dyn_entry(&mut out, DT_STRTAB, strtab_off as u64);
    dyn_entry(&mut out, DT_SYMTAB, symtab_off as u64);
    dyn_entry(&mut out, DT_STRSZ, 1);
    dyn_entry(&mut out, DT_SYMENT, syment as u64);
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
        let elf = synthesize(&spec);

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
        let elf = synthesize(&spec);

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
        let elf = synthesize(&spec);

        assert_eq!(&elf[..6], &[0x7f, b'E', b'L', b'F', 2, 2]);
        assert_eq!(&elf[16..20], &[0, 3, 0, 22]);
        assert_eq!(&elf[32..40], 64u64.to_be_bytes());
    }
}
