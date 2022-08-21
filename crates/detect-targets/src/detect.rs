use std::{
    io::{BufRead, Cursor},
    process::Output,
};

use cfg_if::cfg_if;
use tokio::process::Command;

cfg_if! {
    if #[cfg(target_os = "linux")] {
        mod linux;
    } else if #[cfg(target_os = "macos")] {
        mod macos;
    } else if #[cfg(target_os = "windows")] {
        mod windows;
    }
}

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
pub async fn detect_targets() -> Vec<String> {
    if let Some(target) = get_target_from_rustc().await {
        let mut v = vec![target];

        #[cfg(target_os = "linux")]
        if v[0].contains("gnu") {
            v.push(v[0].replace("gnu", "musl"));
        }

        #[cfg(target_os = "macos")]
        if &*v[0] == macos::AARCH64 {
            v.push(macos::X86.into());
        }

        #[cfg(target_os = "windows")]
        v.extend(windows::detect_alternative_targets(&v[0]));

        v
    } else {
        #[cfg(target_os = "linux")]
        {
            linux::detect_targets_linux().await
        }
        #[cfg(target_os = "macos")]
        {
            macos::detect_targets_macos()
        }
        #[cfg(target_os = "windows")]
        {
            windows::detect_targets_windows()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            vec![TARGET.into()]
        }
    }
}

/// Figure out what the host target is using `rustc`.
/// If `rustc` is absent, then it would return `None`.
async fn get_target_from_rustc() -> Option<String> {
    let Output { status, stdout, .. } = Command::new("rustc").arg("-vV").output().await.ok()?;
    if !status.success() {
        return None;
    }

    Cursor::new(stdout)
        .lines()
        .filter_map(|line| line.ok())
        .find_map(|line| line.strip_prefix("host: ").map(|host| host.to_owned()))
}
