/// Compiled target triple, used as default for binary fetching
pub const TARGET: &str = env!("TARGET");

/// Detect the targets supported at runtime,
/// which might be different from `TARGET` which is detected
/// at compile-time.
///
/// Return targets supported in the order of preference.
/// If target_os is linux and it support gnu, then it is preferred
/// to musl.
///
/// If target_os is mac and it is aarch64, then aarch64 is preferred
/// to x86_64.
///
/// Check [this issue](https://github.com/ryankurte/cargo-binstall/issues/155)
/// for more information.
pub async fn detect_targets() -> Vec<Box<str>> {
    #[cfg(target_os = "linux")]
    {
        return linux::detect_targets_linux().await;
    }
    #[cfg(target_os = "macos")]
    {
        macos::detect_targets_macos()
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::TARGET;
    use std::process::Output;
    use tokio::process::Command;

    async fn detect_targets_linux() -> Vec<Box<str>> {
        let abi = parse_abi();

        if let Ok(Output {
            status: _,
            stdout,
            stderr,
        }) = Command::new("ldd").arg("--version").output().await
        {
            let libc_version = if let Some(libc_version) = parse_libc_version(stdout) {
                libc_version
            } else if let Some(libc_version) = parse_libc_version(stderr) {
                libc_version
            } else {
                return vec![create_target_str("musl", abi)];
            };

            if libc_version == "gnu" {
                return vec![
                    create_target_str("gnu", abi),
                    create_target_str("musl", abi),
                ];
            }
        }

        // Fallback to using musl
        vec![create_target_str("musl", abi)]
    }

    fn parse_libc_version(output: &[u8]) -> Option<&'static str> {
        let s = String::from_utf8_lossy(output);
        if s.contains("musl libc") {
            Some("musl")
        } else if s.contains("GLIBC") {
            Some("gnu")
        } else {
            None
        }
    }

    const fn parse_abi() -> &'static str {
        if TARGET.endswith("abi64") {
            "abi64"
        } else if TARGET.endswith("eabi") {
            "eabi"
        } else if TARGET.endswith("eabihf") {
            "eabihf"
        } else if TARGET.endswith("gnu") || TARGET.endswith("musl") {
            ""
        } else {
            panic!("Unknown abi")
        }
    }

    fn create_target_str(libc_version: &str, abi: &str) -> Box<str> {
        let prefix = TARGET.rsplit_once('-').unwrap().0;

        let mut target = String::with_capacity(prefix.len() + 1 + libc_version.len() + abi.len());
        target.push_str(prefix);
        target.push('-');
        target.push_str(libc_version);
        target.push_str(abi);

        target.into_boxed_str()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use guess_host_triple::guess_host_triple;

    const AARCH64: &str = "aarch64-apple-darwin";
    const X86: &str = "x86_64-apple-darwin";

    pub(super) fn detect_targets_macos() -> Vec<Box<str>> {
        if guess_host_triple() == Some(AARCH64) {
            vec![AARCH64.into(), X86.into()]
        } else {
            vec![X86.into()]
        }
    }
}
