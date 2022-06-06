use arrayvec::ArrayVec;
use std::io::{BufRead, Cursor};
use std::process::Output;
use tokio::process::Command;

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
pub async fn detect_targets() -> ArrayVec<Box<str>, 2> {
    if let Some(target) = get_targets_from_rustc().await {
        return from_array([target]);
    }

    #[cfg(target_os = "linux")]
    {
        linux::detect_targets_linux().await
    }
    #[cfg(target_os = "macos")]
    {
        macos::detect_targets_macos()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        vec![TARGET.into()]
    }
}

// Figure out what the host target is, from rustc or from this program's own build target
async fn get_targets_from_rustc() -> Option<Box<str>> {
    match Command::new("rustc").arg("-vV").output().await {
        Ok(Output { status, stdout, .. }) if status.success() => Cursor::new(stdout)
            .lines()
            .filter_map(|line| line.ok())
            .find_map(|line| {
                line.strip_prefix("host: ")
                    .map(|host| host.to_owned().into_boxed_str())
            }),
        _ => None,
    }
}

fn from_array<T, const LEN: usize, const CAP: usize>(arr: [T; LEN]) -> ArrayVec<T, CAP> {
    let mut v = ArrayVec::new();

    for elem in arr {
        v.push(elem);
    }

    v
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{from_array, ArrayVec, Command, Output, TARGET};

    pub(super) async fn detect_targets_linux() -> ArrayVec<Box<str>, 2> {
        let abi = parse_abi();

        if let Ok(Output {
            status: _,
            stdout,
            stderr,
        }) = Command::new("ldd").arg("--version").output().await
        {
            let libc_version =
                if let Some(libc_version) = parse_libc_version_from_ldd_output(stdout) {
                    libc_version
                } else if let Some(libc_version) = parse_libc_version(stderr) {
                    libc_version
                } else {
                    return from_array([create_target_str("musl", abi)]);
                };

            if libc_version == "gnu" {
                return from_array([
                    create_target_str("gnu", abi),
                    create_target_str("musl", abi),
                ]);
            }
        }

        // Fallback to using musl
        from_array([create_target_str("musl", abi)])
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
    use super::{from_array, ArrayVec};
    use guess_host_triple::guess_host_triple;

    const AARCH64: &str = "aarch64-apple-darwin";
    const X86: &str = "x86_64-apple-darwin";

    pub(super) fn detect_targets_macos() -> ArrayVec<Box<str>, 2> {
        if guess_host_triple() == Some(AARCH64) {
            from_array([AARCH64.into(), X86.into()])
        } else {
            from_array([X86.into()])
        }
    }
}
