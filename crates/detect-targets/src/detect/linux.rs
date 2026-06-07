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

            [has_glibc.then_some(gnu_target), Some(musl_fallback_target())]
        }
        Libc::Android | Libc::Unknown => [Some(target.clone()), Some(musl_fallback_target())],
    }
    .into_iter()
    .flatten()
    .collect()
}

enum Libc {
    Gnu,
    Musl,
    Android,
    Unknown,
}
