use std::process::Stdio;

use tokio::process::Command;

const AARCH64: &str = "aarch64-apple-darwin";
const X86: &str = "x86_64-apple-darwin";
/// https://doc.rust-lang.org/nightly/rustc/platform-support/x86_64h-apple-darwin.html
///
/// This target is an x86_64 target that only supports Apple's late-gen
/// (Haswell-compatible) Intel chips.
///
/// It enables a set of target features available on these chips (AVX2 and similar),
/// and MachO binaries built with this target may be used as the x86_64h entry in
/// universal binaries ("fat" MachO binaries), and will fail to load on machines
/// that do not support this.
///
/// It is similar to x86_64-apple-darwin in nearly all respects, although
/// the minimum supported OS version is slightly higher (it requires 10.8
/// rather than x86_64-apple-darwin's 10.7).
const X86H: &str = "x86_64h-apple-darwin";
const UNIVERSAL: &str = "universal-apple-darwin";
const UNIVERSAL2: &str = "universal2-apple-darwin";

async fn is_arch_supported(arch_name: &str) -> bool {
    Command::new("arch")
        .args(["-arch", arch_name, "/usr/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|exit_status| exit_status.success())
        .unwrap_or(false)
}

pub(super) async fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> {
    match target {
        AARCH64 => {
            // Spawn `arch` in parallel (probably from different threads if
            // mutlti-thread runtime is used).
            //
            // These two tasks are never cancelled, so it can only fail due to
            // panic, in which cause we would propagate by also panic here.
            let x86_64h_task = tokio::spawn(is_arch_supported("x86_64h"));
            let x86_64_task = tokio::spawn(is_arch_supported("x86_64"));
            [
                // Prefer universal as it provides native arm executable
                Some(UNIVERSAL),
                Some(UNIVERSAL2),
                // Prefer x86h since it is more optimized
                x86_64h_task.await.unwrap().then_some(X86H),
                x86_64_task.await.unwrap().then_some(X86),
            ]
        }
        X86 => [
            is_arch_supported("x86_64h").await.then_some(X86H),
            Some(UNIVERSAL),
            Some(UNIVERSAL2),
            None,
        ],
        X86H => [Some(X86), Some(UNIVERSAL), Some(UNIVERSAL2), None],
        _ => [None, None, None, None],
    }
    .into_iter()
    .flatten()
    .map(ToString::to_string)
}
