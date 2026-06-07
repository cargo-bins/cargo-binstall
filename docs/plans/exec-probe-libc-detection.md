# Exec-probe libc/loader detection for detect-targets

## Problem

`detect-targets` decides whether a Linux system supports gnu (glibc) targets by
probing a hardcoded list of dynamic-loader paths and string-matching the output
of `<ld.so> --version` (`crates/detect-targets/src/detect/linux.rs`). This is
fragile:

- Gentoo rebrands the version banner (`ld.so (Gentoo 2.42-r7 (patchset 9)) ...`),
  so a real glibc system is detected as musl-only
  ([#1999](https://github.com/cargo-bins/cargo-binstall/issues/1999)).
- armv7 needed an empirical extra-paths hack because the constructed loader
  filename `ld-linux-armv7.so.2` doesn't exist
  ([#2386](https://github.com/cargo-bins/cargo-binstall/issues/2386)) — the real
  hard-float loader is `ld-linux-armhf.so.3`.
- Alpine gcompat, Ubuntu 20.04's exit-127, and musl-on-stderr each need their
  own special case in `get_ld_flavor`.

## Approach

Apply the `detect-wasi` technique to libc detection: synthesize a minimal
dynamically-linked ELF whose `PT_INTERP` is the per-arch ABI-standard loader
path and whose entry point is an `exit_group(0)` syscall stub, write it to a
temp dir, exec it, and observe the result. The probe exercises the exact
mechanism a real downloaded binary uses, so it cannot disagree with reality:

- clean exit 0 → that loader works → targets linked against it are runnable;
- `ENOENT` from execve (we just created the file, so the missing thing is the
  interpreter) or `ENOEXEC` (no binfmt handler for a foreign arch) or unclean
  exit → not runnable;
- `EACCES`/`EPERM`/other (noexec tmp, SELinux, seccomp) → *inconclusive* —
  fall back to the existing path-probe + string-match logic, so restricted
  environments degrade to today's behavior, never worse.

This removes the string matching entirely for the common case, handles gcompat
naturally (its `/lib/ld-linux-x86-64.so.2` works as an interpreter, so the
probe succeeds — correctly), and keys ABI variants off the loader *path*
(armhf vs soft-float, lp64d, multilib) rather than banner text.

ELFs are synthesized at runtime in plain Rust (commented hex in source) —
no committed binary blobs, no build.rs cross-compilation, no new heavyweight
deps. A no-`DT_NEEDED` probe (~200 bytes) tests loader presence, which is
sufficient: the loader ships in the libc package.

## Deliverables

1. **`detect_targets::probe` module** (public, `cfg(any(target_os = "linux", target_os = "android"))`):
   - `elf.rs` — minimal-ELF writer: ELF32/ELF64 × LE/BE, ELF header +
     `PT_LOAD` (RX, covers headers + stub + strings) + `PT_INTERP` +
     `PT_DYNAMIC` containing only `DT_NULL` (musl's loader may want a dynamic
     segment; empty means "nothing to resolve" — **verify on Alpine in CI**,
     see Risks) + per-arch exit stub. No section headers.
   - `table.rs` — static probe table mapping Rust target triples to
     `(class, endian, e_machine, e_flags, interp path, stub)`.
   - `mod.rs` — `Probe { target: &'static str, .. }`,
     `ProbeResult { Runnable, NotRunnable, Inconclusive(io::Error) }`,
     `Probe::run() -> ProbeResult` (async, tokio), `probes() -> &'static [Probe]`,
     plus a host-native subset helper.
2. **Rewritten `detect/linux.rs`**: the Gnu/Musl arm runs the gnu probe for the
   host arch+abi; `Runnable` → `[gnu, musl-fallback]`, `NotRunnable` →
   `[musl-fallback]`, `Inconclusive` → legacy path-probe logic (moved to
   `detect/linux/fallback.rs`, unchanged in behavior). Output shape of
   `detect_targets()` is **unchanged** — same-arch gnu/musl ordering only —
   so binstalk needs no changes and is wired automatically. Android arm keeps
   current behavior.
3. **CLI**: `detect-targets --probe-all` flag on the existing bin prints every
   table entry with its probe result — debugging aid + CI harness.
4. **CI**:
   - Add `gentoo/stage3` to the `detect-targets-more-glibc-test` matrix
     (expects `x86_64-unknown-linux-gnu` + musl) — the #1999 regression test.
   - All existing distro jobs (alpine, ubuntu, fedora, arch, nix musl-only)
     must keep passing unchanged — they now validate the probe path.
   - New job: `--probe-all` in an `i386/debian` container — validates the
     multilib surface (i686 gnu Runnable, x86_64 gnu NotRunnable there).
5. **Tests** (in-crate):
   - Golden-bytes test for the x86_64 gnu probe ELF (validated against
     readelf during development).
   - Host-exec test: on a gnu CI host, the host gnu probe is `Runnable`; the
     uClibc probe is `NotRunnable`.

## Probe table

Stubs are per-CPU-arch `exit_group(0)`; encodings verified against an
assembler during implementation, committed as commented hex.

| arch (stub) | syscall nr | notes |
|---|---|---|
| x86_64 | 231 | `mov eax,231; xor edi,edi; syscall` |
| x86_64 (x32 ABI) | 0x40000000+231 | same insns, x32 bit set |
| i686 | 252 | `int 0x80` |
| aarch64 | 94 | `mov x8,#94; mov x0,#0; svc #0` |
| arm (EABI, v7) | 248 | `mov r7,#248; mov r0,#0; svc #0` |
| riscv64 | 94 | `li a7,94; li a0,0; ecall` |
| powerpc64le | 234 | `li r0,234; li r3,0; sc` |
| s390x (BE) | 248 | `lghi r2,0; svc 248`-equivalent |
| loongarch64 | 94 | `li.w a7,94; li.w a0,0; syscall 0` |

Interp paths (per table entry; e_flags per-arch where ABI-relevant, e.g.
EABI5+hard-float for armhf, RVC+double-float for riscv64gc):

- **glibc**: x86_64 `/lib64/ld-linux-x86-64.so.2`; i686 `/lib/ld-linux.so.2`;
  x32 `/libx32/ld-linux-x32.so.2`; aarch64 `/lib/ld-linux-aarch64.so.1`;
  armv7hf `/lib/ld-linux-armhf.so.3`; arm sf `/lib/ld-linux.so.3`;
  riscv64gc `/lib/ld-linux-riscv64-lp64d.so.1`; ppc64le `/lib64/ld64.so.2`;
  s390x `/lib/ld64.so.1`; loongarch64 `/lib64/ld-linux-loongarch-lp64d.so.1`
- **musl (dynamic)**: `/lib/ld-musl-{x86_64,i386,aarch64,armhf,arm,riscv64,powerpc64le,s390x,loongarch64}.so.1`
- **uClibc-ng**: `/lib/ld-uClibc.so.0` (32-bit), `/lib/ld64-uClibc.so.0` (64-bit)
- **bionic**: `/system/bin/linker64` (64-bit), `/system/bin/linker` (32-bit)

Every entry is keyed by the Rust target triple it attests
(e.g. `i686-unknown-linux-gnu`, `armv7-unknown-linux-musleabihf`,
`aarch64-linux-android`). Foreign-arch probes come for free: running a
non-native entry succeeds iff a binfmt handler (qemu-user etc.) plus that
libc are installed — same property detect-wasi documents for wasi binfmt.

Deferred (not in this round): mips*, ppc64 BE, aarch64_be, riscv32 table
entries; wiring multilib/foreign-arch/bionic/uClibc results into
`detect_targets()` output and binstall target resolution.

## Risks / verification points

- **musl loader vs empty PT_DYNAMIC**: if Alpine CI shows musl's ld rejecting
  a dynamic segment with only DT_NULL, fall back to omitting PT_DYNAMIC for
  musl probes (kernel + loader treat it as static-started-via-interp), or add
  minimal hash/strtab structures. Resolve empirically in CI before merging.
- **e_flags strictness**: kernels don't validate e_flags on exec, but loaders
  may (armhf). Copy e_flags from real reference binaries via readelf.
- **noexec $TMPDIR**: handled by Inconclusive → legacy fallback. detect-wasi
  has the same blind spot and lives with it; we do better.
- **False negative direction**: a probe failure on a weird-but-working system
  degrades to musl-fallback (today's Gentoo bug behavior) — never a hard break,
  since musl artifacts are static.
- **MSRV**: crate declares 1.62; no CI gate, but avoid gratuitously modern
  constructs.

## Dependencies

- Add `tempfile` (already used by detect-wasi at 3.5.0) to detect-targets.
- No other new deps; ELF writer is hand-rolled (no `object` crate).

## Step plan

1. `probe/elf.rs` writer + golden test (x86_64 gnu) — verify with readelf.
2. `probe/table.rs` stubs + entries for binstall's shipped arches
   (x86_64, i686, aarch64, armv7 hf/sf, riscv64) × (gnu, musl), then the
   extended surface (x32, ppc64le, s390x, loongarch64, uClibc, bionic).
3. `probe/mod.rs` runner + result classification + public API.
4. `--probe-all` CLI flag.
5. Rewire `detect/linux.rs` (probe first, legacy fallback on Inconclusive);
   move legacy logic to `detect/linux/fallback.rs`.
6. CI: gentoo job, i386/debian probe-all job.
7. Crate docs update (lib.rs doc comment still says "syscalls plus ldd").
