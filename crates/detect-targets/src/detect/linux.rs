use crate::probe::{self, ProbeResult};

#[cfg(feature = "tracing")]
use tracing::debug;

mod fallback;

pub(super) async fn detect_targets(target: String) -> Vec<String> {
    let (_, postfix) = target
        .rsplit_once('-')
        .expect("unwrap: target always has a -");

    let (abi, libc) = if let Some(abi) = postfix.strip_prefix("musl") {
        (abi, Libc::Musl)
    } else if let Some(abi) = postfix.strip_prefix("gnu") {
        (abi, Libc::Gnu)
    } else if let Some(abi) = postfix.strip_prefix("android") {
        (abi, Libc::Android)
    } else {
        (postfix, Libc::Unknown)
    };

    let cpu_arch = target
        .split_once('-')
        .expect("unwrap: target always has a - for cpu_arch")
        .0;

    // For android the `-unknown-` is omitted, for alpine it has `-alpine-`
    // instead of `-unknown-`.
    let musl_fallback_target = || format!("{cpu_arch}-unknown-linux-musl{abi}");

    match libc {
        // guess_host_triple cannot detect whether the system is using glibc,
        // musl libc or other libc, and the compile-time libc may not match
        // the runtime environment: a musl build runs fine on a glibc distro,
        // and Alpine can run glibc programs via `apk add gcompat`.
        //
        // Test for a working glibc by executing a synthesized probe binary
        // whose PT_INTERP is the ABI-standard glibc loader path — the same
        // mechanism any real gnu binary uses. If the environment prevents
        // the test from running at all (e.g. noexec temp dir, seccomp),
        // fall back to probing known loader paths and parsing their
        // `--version` banners.
        Libc::Gnu | Libc::Musl => {
            let gnu_target = format!("{cpu_arch}-unknown-linux-gnu{abi}");

            let has_glibc = match probe::find(&gnu_target) {
                Some(probe) => match probe.run().await {
                    ProbeResult::Runnable => true,
                    ProbeResult::NotRunnable => false,
                    ProbeResult::Inconclusive(_err) => {
                        #[cfg(feature = "tracing")]
                        debug!(
                            "glibc probe inconclusive ({_err}), \
                             falling back to loader path detection"
                        );
                        fallback::has_glibc(cpu_arch, abi).await
                    }
                },
                None => fallback::has_glibc(cpu_arch, abi).await,
            };

            let compat_targets = detect_extra_targets(cpu_arch, abi).await;

            [has_glibc.then_some(gnu_target), Some(musl_fallback_target())]
                .into_iter()
                .flatten()
                .chain(compat_targets)
                .collect()
        }
        Libc::Android | Libc::Unknown => vec![target.clone(), musl_fallback_target()],
    }
}

/// Cross-arch / cross-ABI targets that may also run on this machine,
/// in preference order, each verified by a loader probe. These are
/// appended after the native targets, so they are only used when no
/// native artifact is available.
///
/// Two tiers, ranked in that order:
///
/// 1. compat: targets the kernel runs natively alongside the host
///    arch (ia32 on x86_64, aarch32 on arm64, soft-float on
///    hard-float, a 64-bit kernel under a 32-bit userland);
/// 2. foreign: targets that only run through a binfmt handler such as
///    qemu-user. Emulation is slow, but a runnable artifact still
///    beats no artifact at all when nothing else is available.
///
/// gnu candidates are gated on the dynamic probe (they need the glibc
/// loader, e.g. multilib or a multiarch/sysroot setup); musl
/// candidates on the static probe, since Rust musl artifacts are
/// typically statically linked and only need the kernel (or its
/// binfmt handler) to support the architecture.
async fn detect_extra_targets(cpu_arch: &str, abi: &str) -> Vec<String> {
    let compat: &[&str] = match (cpu_arch, abi) {
        // 64-bit kernels usually retain compat support for their
        // 32-bit predecessors.
        ("x86_64", _) => &["i686-unknown-linux-gnu", "i686-unknown-linux-musl"],
        ("aarch64", _) => &[
            "armv7-unknown-linux-gnueabihf",
            "armv7-unknown-linux-musleabihf",
        ],
        // An i686 userland may be running on an x86_64 kernel.
        ("i686", _) => &["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"],
        // Soft-float binaries run fine on hard-float systems. The
        // reverse cannot be probed: the probe stub exercises no FPU,
        // so it cannot attest hard-float support on a soft-float host.
        ("armv7", "eabihf") => &["armv7-unknown-linux-gnueabi", "armv7-unknown-linux-musleabi"],
        _ => &[],
    };

    const FOREIGN: &[&str] = &[
        "x86_64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        "armv7-unknown-linux-gnueabihf",
        "armv7-unknown-linux-musleabihf",
        "riscv64gc-unknown-linux-gnu",
        "riscv64gc-unknown-linux-musl",
        "powerpc64le-unknown-linux-gnu",
        "powerpc64le-unknown-linux-musl",
        "s390x-unknown-linux-gnu",
        "s390x-unknown-linux-musl",
        "loongarch64-unknown-linux-gnu",
        "loongarch64-unknown-linux-musl",
        "i686-unknown-linux-gnu",
        "i686-unknown-linux-musl",
    ];

    let native_prefix = format!("{cpu_arch}-");
    let candidates = compat.iter().chain(
        FOREIGN
            .iter()
            // native targets are handled by the caller, and compat
            // targets have already been listed in the first tier
            .filter(|t| !t.starts_with(&native_prefix) && !compat.contains(t)),
    );

    // Each probe is an exec of a tiny binary; run them all
    // concurrently and collect the results in candidate order.
    let handles: Vec<_> = candidates
        .map(|&candidate| {
            tokio::spawn(async move {
                let probe = probe::find(candidate)?;
                let result = if candidate.contains("-musl") {
                    probe.run_static().await
                } else {
                    probe.run().await
                };
                matches!(result, ProbeResult::Runnable).then(|| candidate.to_string())
            })
        })
        .collect();

    let mut targets = Vec::new();
    for handle in handles {
        if let Ok(Some(target)) = handle.await {
            targets.push(target);
        }
    }
    targets
}

enum Libc {
    Gnu,
    Musl,
    Android,
    Unknown,
}
