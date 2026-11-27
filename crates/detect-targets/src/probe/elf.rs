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

use goblin::{
    container::{Container, Ctx},
    elf::{
        Dyn, Header, ProgramHeader, Sym,
        dynamic::{DT_HASH, DT_NULL, DT_STRSZ, DT_STRTAB, DT_SYMENT, DT_SYMTAB},
        header::ET_DYN,
        program_header::{PF_R, PF_W, PF_X, PT_DYNAMIC, PT_GNU_STACK, PT_INTERP, PT_LOAD, PT_PHDR},
    },
};
use scroll::{
    Pwrite,
    ctx::{IntoCtx, SizeWith},
};

/// ELF file class, `e_ident[EI_CLASS]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Class {
    Elf32 = 1,
    Elf64 = 2,
}

impl Class {
    /// goblin's parsing context for this class and `endian`.
    pub(crate) fn ctx(self, endian: Endian) -> Ctx {
        Ctx::new(
            match self {
                Class::Elf32 => Container::Little,
                Class::Elf64 => Container::Big,
            },
            match endian {
                Endian::Little => scroll::Endian::Little,
                Endian::Big => scroll::Endian::Big,
            },
        )
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

/// Synthesize the probe executable described by `spec`.
///
/// With `with_interp` set, the executable carries a `PT_INTERP` and
/// running it exercises the dynamic loader at `spec.interp`. Without
/// it, the executable is static: the kernel jumps straight to the
/// stub, so a clean exit only proves the kernel can execute this
/// architecture (natively, via compat mode, or through a binfmt
/// handler).
pub(crate) fn synthesize(spec: &ElfSpec, with_interp: bool) -> Vec<u8> {
    let ctx = spec.class.ctx(spec.endian);
    let ehsize = Header::size(ctx);
    let phentsize = ProgramHeader::size(ctx);
    let dynent = Dyn::size_with(&ctx);
    let syment = Sym::size_with(&ctx);

    // PT_PHDR, PT_LOAD, [PT_INTERP,] PT_DYNAMIC, PT_GNU_STACK
    //
    // PT_PHDR is required: it is how the dynamic loader computes the
    // load base of an ET_DYN main program (l_addr = AT_PHDR -
    // PT_PHDR.p_vaddr); without it glibc assumes a load base of 0 and
    // crashes dereferencing unrelocated p_vaddr values.
    let phnum = if with_interp { 5usize } else { 4 };

    // File layout. Since this is ET_DYN, virtual addresses equal file
    // offsets (single PT_LOAD mapping the whole file at base 0).
    let phoff = ehsize;
    let interp_off = phoff + phnum * phentsize;
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
    let strtab_off = symtab_off + syment;
    let dyn_off = (strtab_off + 1).next_multiple_of(8);
    let dynamic = [
        (DT_HASH, hash_off),
        (DT_STRTAB, strtab_off),
        (DT_SYMTAB, symtab_off),
        (DT_STRSZ, 1),
        (DT_SYMENT, syment),
        (DT_NULL, 0),
    ];
    let dyn_size = dynamic.len() * dynent;
    let stub_off = (dyn_off + dyn_size).next_multiple_of(16);
    let total = stub_off + spec.stub.len();

    // Everything not written below (padding, the null symbol, the
    // one-NUL string table, the hash table's bucket and chain) is zero.
    let mut buf = vec![0; total];
    // `buf` is sized for this layout, so no write can run out of room.
    const FITS: &str = "the probe buffer is sized for its layout";

    let mut header = Header::new(ctx);
    header.e_type = ET_DYN;
    header.e_machine = spec.e_machine;
    header.e_entry = stub_off as u64;
    header.e_phoff = phoff as u64;
    header.e_flags = spec.e_flags;
    header.e_phnum = phnum as u16;
    // No section headers.
    header.e_shentsize = 0;
    header.into_ctx(&mut buf, ctx);

    // A segment whose virtual address equals its file offset, and whose
    // size in memory equals its size in the file.
    let segment = |p_type, p_flags, offset: usize, size: usize, p_align| ProgramHeader {
        p_type,
        p_flags,
        p_offset: offset as u64,
        p_vaddr: offset as u64,
        p_paddr: offset as u64,
        p_filesz: size as u64,
        p_memsz: size as u64,
        p_align,
    };
    let phdrs = [
        Some(segment(PT_PHDR, PF_R, phoff, phnum * phentsize, 8)),
        Some(segment(PT_LOAD, PF_R | PF_X, 0, total, 0x1000)),
        with_interp.then(|| segment(PT_INTERP, PF_R, interp_off, interp_size, 1)),
        Some(segment(PT_DYNAMIC, PF_R, dyn_off, dyn_size, 8)),
        Some(segment(PT_GNU_STACK, PF_R | PF_W, 0, 0, 0x10)),
    ];
    for (i, phdr) in phdrs.into_iter().flatten().enumerate() {
        let off = phoff + i * phentsize;
        buf.pwrite_with(phdr, off, ctx).expect(FITS);
    }

    if with_interp {
        // NUL-terminated by the zeroed byte that follows.
        buf[interp_off..interp_off + spec.interp.len()].copy_from_slice(spec.interp.as_bytes());
    }

    // Hash table: nbucket = nchain = 1.
    let le = ctx.le;
    buf.pwrite_with(1u32, hash_off, le).expect(FITS);
    buf.pwrite_with(1u32, hash_off + 4, le).expect(FITS);

    for (i, (d_tag, d_val)) in dynamic.into_iter().enumerate() {
        let off = dyn_off + i * dynent;
        let entry = Dyn {
            d_tag,
            d_val: d_val as u64,
        };
        buf.pwrite_with(entry, off, ctx).expect(FITS);
    }

    buf[stub_off..].copy_from_slice(spec.stub);
    buf
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
