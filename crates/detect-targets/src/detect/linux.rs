use crate::TARGET;

use std::process::{Output, Stdio};

use guess_host_triple::guess_host_triple;
use tokio::process::Command;

pub(super) async fn detect_targets_linux() -> Vec<String> {
    let target = guess_host_triple().unwrap_or(TARGET);

    let (prefix, postfix) = target
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

    let musl_fallback_target = || format!("{prefix}-{}{abi}", "musl");

    match libc {
        Libc::Gnu => {
            // guess_host_triple cannot detect whether the system is using glibc,
            // musl libc or other libc.
            //
            // As such, we need to launch the test ourselves.
            if supports_gnu().await {
                vec![target.to_string(), musl_fallback_target()]
            } else {
                vec![musl_fallback_target()]
            }
        }
        Libc::Android => vec![target.to_string(), musl_fallback_target()],

        _ => vec![target.to_string()],
    }
}

async fn supports_gnu() -> bool {
    Command::new("ldd")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()
        .and_then(|Output { stdout, stderr, .. }| {
            parse_libc_version_from_ldd_output(&stdout)
                .or_else(|| parse_libc_version_from_ldd_output(&stderr))
        })
        == Some("gnu")
}

fn parse_libc_version_from_ldd_output(output: &[u8]) -> Option<&'static str> {
    let s = String::from_utf8_lossy(output);
    if s.contains("musl libc") {
        Some("musl")
    } else if s.contains("GLIBC") {
        Some("gnu")
    } else {
        None
    }
}

enum Libc {
    Gnu,
    Musl,
    Android,
    Unknown,
}
