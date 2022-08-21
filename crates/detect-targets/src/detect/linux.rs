use crate::TARGET;

use std::process::{Output, Stdio};

use tokio::process::Command;

pub(super) async fn detect_targets_linux() -> Vec<String> {
    let (abi, libc) = parse_abi_and_libc();

    if let Libc::Glibc = libc {
        // Glibc can only be dynamically linked.
        // If we can run this binary, then it means that the target
        // supports both glibc and musl.
        return create_targets_str(&["gnu", "musl"], abi);
    }

    if let Ok(Output {
        status: _,
        stdout,
        stderr,
    }) = Command::new("ldd").arg("--version").stdin(Stdio::null()).output().await
    {
        let libc_version = if let Some(libc_version) = parse_libc_version_from_ldd_output(&stdout) {
            libc_version
        } else if let Some(libc_version) = parse_libc_version_from_ldd_output(&stderr) {
            libc_version
        } else {
            return vec![create_target_str("musl", abi)];
        };

        if libc_version == "gnu" {
            return create_targets_str(&["gnu", "musl"], abi);
        }
    }

    // Fallback to using musl
    vec![create_target_str("musl", abi)]
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
    Glibc,
    Musl,
}

fn parse_abi_and_libc() -> (&'static str, Libc) {
    let last = TARGET.rsplit_once('-').unwrap().1;

    if let Some(libc_version) = last.strip_prefix("musl") {
        (libc_version, Libc::Musl)
    } else if let Some(libc_version) = last.strip_prefix("gnu") {
        (libc_version, Libc::Glibc)
    } else {
        panic!("Unrecognized libc")
    }
}

fn create_target_str(libc_version: &str, abi: &str) -> String {
    let prefix = TARGET
        .rsplit_once('-')
        .expect("unwrap: TARGET always has a -")
        .0;

    format!("{prefix}-{libc_version}{abi}")
}

fn create_targets_str(libc_versions: &[&str], abi: &str) -> Vec<String> {
    libc_versions
        .iter()
        .map(|libc_version| create_target_str(libc_version, abi))
        .collect()
}
