//! The table of known loader probes.
//!
//! Each entry maps a Rust target triple to the synthesized ELF that
//! attests it: CPU architecture (ELF class/endianness/machine/flags +
//! `exit_group(0)` stub) plus the ABI-standard `PT_INTERP` path of the
//! libc flavour. Stub encodings were produced by clang's integrated
//! assembler and are committed as commented hex.

use super::elf::{Class, ElfSpec, Endian};
use super::Probe;

/// `mov eax, 231; xor edi, edi; syscall`
const STUB_X86_64: &[u8] = &[0xb8, 0xe7, 0x00, 0x00, 0x00, 0x31, 0xff, 0x0f, 0x05];

/// `mov eax, 0x40000000|231; xor edi, edi; syscall` (x32 syscall ABI)
const STUB_X32: &[u8] = &[0xb8, 0xe7, 0x00, 0x00, 0x40, 0x31, 0xff, 0x0f, 0x05];

/// `xor ebx, ebx; mov eax, 252; int 0x80`
const STUB_I686: &[u8] = &[0x31, 0xdb, 0xb8, 0xfc, 0x00, 0x00, 0x00, 0xcd, 0x80];

/// `mov x0, #0; mov x8, #94; svc #0`
const STUB_AARCH64: &[u8] = &[
    0x00, 0x00, 0x80, 0xd2, // mov x0, #0
    0xc8, 0x0b, 0x80, 0xd2, // mov x8, #94
    0x01, 0x00, 0x00, 0xd4, // svc #0
];

/// `mov r0, #0; mov r7, #248; svc #0` (EABI)
const STUB_ARM: &[u8] = &[
    0x00, 0x00, 0xa0, 0xe3, // mov r0, #0
    0xf8, 0x70, 0xa0, 0xe3, // mov r7, #248
    0x00, 0x00, 0x00, 0xef, // svc #0
];

/// `li a0, 0; li a7, 94; ecall` (uncompressed, valid with or without RVC)
const STUB_RISCV64: &[u8] = &[
    0x13, 0x05, 0x00, 0x00, // li a0, 0
    0x93, 0x08, 0xe0, 0x05, // li a7, 94
    0x73, 0x00, 0x00, 0x00, // ecall
];

/// `li r0, 234; li r3, 0; sc`
const STUB_PPC64LE: &[u8] = &[
    0xea, 0x00, 0x00, 0x38, // li r0, 234
    0x00, 0x00, 0x60, 0x38, // li r3, 0
    0x02, 0x00, 0x00, 0x44, // sc
];

/// `lghi %r2, 0; svc 248` (big-endian)
const STUB_S390X: &[u8] = &[
    0xa7, 0x29, 0x00, 0x00, // lghi %r2, 0
    0x0a, 0xf8, // svc 248
];

/// `ori $a0, $zero, 0; ori $a7, $zero, 94; syscall 0`
const STUB_LOONGARCH64: &[u8] = &[
    0x04, 0x00, 0x80, 0x03, // ori $a0, $zero, 0
    0x0b, 0x78, 0x81, 0x03, // ori $a7, $zero, 94
    0x00, 0x00, 0x2b, 0x00, // syscall 0
];

// e_machine values
const EM_386: u16 = 3;
const EM_PPC64: u16 = 21;
const EM_S390: u16 = 22;
const EM_ARM: u16 = 40;
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;
const EM_RISCV: u16 = 243;
const EM_LOONGARCH: u16 = 258;

// e_flags of linked executables, as observed via readelf on real
// binaries of each ABI.
/// Version5 EABI, hard-float ABI
const EF_ARM_HF: u32 = 0x0500_0400;
/// Version5 EABI, soft-float ABI
const EF_ARM_SF: u32 = 0x0500_0200;
/// RVC, double-float ABI
const EF_RISCV64GC: u32 = 0x5;
/// ABI v2 (ELFv2, always used by ppc64le)
const EF_PPC64_V2: u32 = 0x2;
/// DOUBLE-FLOAT, OBJ-v1
const EF_LOONGARCH64: u32 = 0x43;

macro_rules! probe {
    ($target:literal, $class:ident, $endian:ident, $machine:ident, $flags:expr, $interp:literal, $stub:ident) => {
        Probe {
            target: $target,
            spec: ElfSpec {
                class: Class::$class,
                endian: Endian::$endian,
                e_machine: $machine,
                e_flags: $flags,
                interp: $interp,
                stub: $stub,
            },
        }
    };
}

/// All known probes, grouped by libc flavour.
///
/// Note that x32 binaries are ELFCLASS32 with `EM_X86_64`.
// Deliberately tabular, one row per probe: kept out of rustfmt's
// hands so the table stays scannable.
#[rustfmt::skip]
pub(super) const PROBES: &[Probe] = &[
    // glibc — the loader path is the per-arch ELF ABI standard one,
    // which every prebuilt gnu binary hardcodes, so distros must
    // provide it no matter where the real file lives.
    probe!("x86_64-unknown-linux-gnu", Elf64, Little, EM_X86_64, 0, "/lib64/ld-linux-x86-64.so.2", STUB_X86_64),
    probe!("i686-unknown-linux-gnu", Elf32, Little, EM_386, 0, "/lib/ld-linux.so.2", STUB_I686),
    probe!("x86_64-unknown-linux-gnux32", Elf32, Little, EM_X86_64, 0, "/libx32/ld-linux-x32.so.2", STUB_X32),
    probe!("aarch64-unknown-linux-gnu", Elf64, Little, EM_AARCH64, 0, "/lib/ld-linux-aarch64.so.1", STUB_AARCH64),
    probe!("armv7-unknown-linux-gnueabihf", Elf32, Little, EM_ARM, EF_ARM_HF, "/lib/ld-linux-armhf.so.3", STUB_ARM),
    probe!("armv7-unknown-linux-gnueabi", Elf32, Little, EM_ARM, EF_ARM_SF, "/lib/ld-linux.so.3", STUB_ARM),
    probe!("riscv64gc-unknown-linux-gnu", Elf64, Little, EM_RISCV, EF_RISCV64GC, "/lib/ld-linux-riscv64-lp64d.so.1", STUB_RISCV64),
    probe!("powerpc64le-unknown-linux-gnu", Elf64, Little, EM_PPC64, EF_PPC64_V2, "/lib64/ld64.so.2", STUB_PPC64LE),
    probe!("s390x-unknown-linux-gnu", Elf64, Big, EM_S390, 0, "/lib/ld64.so.1", STUB_S390X),
    probe!("loongarch64-unknown-linux-gnu", Elf64, Little, EM_LOONGARCH, EF_LOONGARCH64, "/lib64/ld-linux-loongarch-lp64d.so.1", STUB_LOONGARCH64),
    // musl (dynamic linking) — note musl spells the arch uname-style
    // (x86_64, i386, armhf) rather than the gnu way (x86-64, armhf.so.3).
    probe!("x86_64-unknown-linux-musl", Elf64, Little, EM_X86_64, 0, "/lib/ld-musl-x86_64.so.1", STUB_X86_64),
    probe!("i686-unknown-linux-musl", Elf32, Little, EM_386, 0, "/lib/ld-musl-i386.so.1", STUB_I686),
    probe!("aarch64-unknown-linux-musl", Elf64, Little, EM_AARCH64, 0, "/lib/ld-musl-aarch64.so.1", STUB_AARCH64),
    probe!("armv7-unknown-linux-musleabihf", Elf32, Little, EM_ARM, EF_ARM_HF, "/lib/ld-musl-armhf.so.1", STUB_ARM),
    probe!("armv7-unknown-linux-musleabi", Elf32, Little, EM_ARM, EF_ARM_SF, "/lib/ld-musl-arm.so.1", STUB_ARM),
    probe!("riscv64gc-unknown-linux-musl", Elf64, Little, EM_RISCV, EF_RISCV64GC, "/lib/ld-musl-riscv64.so.1", STUB_RISCV64),
    probe!("powerpc64le-unknown-linux-musl", Elf64, Little, EM_PPC64, EF_PPC64_V2, "/lib/ld-musl-powerpc64le.so.1", STUB_PPC64LE),
    probe!("s390x-unknown-linux-musl", Elf64, Big, EM_S390, 0, "/lib/ld-musl-s390x.so.1", STUB_S390X),
    probe!("loongarch64-unknown-linux-musl", Elf64, Little, EM_LOONGARCH, EF_LOONGARCH64, "/lib/ld-musl-loongarch64.so.1", STUB_LOONGARCH64),
    // uClibc-ng
    probe!("armv7-unknown-linux-uclibceabihf", Elf32, Little, EM_ARM, EF_ARM_HF, "/lib/ld-uClibc.so.0", STUB_ARM),
    probe!("armv7-unknown-linux-uclibceabi", Elf32, Little, EM_ARM, EF_ARM_SF, "/lib/ld-uClibc.so.0", STUB_ARM),
    // bionic (Android)
    probe!("x86_64-linux-android", Elf64, Little, EM_X86_64, 0, "/system/bin/linker64", STUB_X86_64),
    probe!("i686-linux-android", Elf32, Little, EM_386, 0, "/system/bin/linker", STUB_I686),
    probe!("aarch64-linux-android", Elf64, Little, EM_AARCH64, 0, "/system/bin/linker64", STUB_AARCH64),
    probe!("armv7-linux-androideabi", Elf32, Little, EM_ARM, EF_ARM_SF, "/system/bin/linker", STUB_ARM),
];
