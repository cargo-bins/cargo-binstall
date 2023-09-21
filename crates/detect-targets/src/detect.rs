use std::{
    borrow::Cow,
    env,
    ffi::OsStr,
    process::{Output, Stdio},
};

use cfg_if::cfg_if;
use tokio::process::Command;
use tracing::debug;

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
    let target = get_target_from_rustc().await;
    debug!("get_target_from_rustc()={target:?}");
    let target = target.unwrap_or_else(|| {
        let target = guess_host_triple::guess_host_triple();
        debug!("guess_host_triple::guess_host_triple()={target:?}");
        target.unwrap_or(crate::TARGET).to_string()
    });

    cfg_if! {
        if #[cfg(target_os = "macos")] {
            let mut targets = vec![target];
            targets.extend(macos::detect_alternative_targets(&targets[0]).await);
            targets
        } else if #[cfg(target_os = "windows")] {
            let mut targets = vec![target];
            targets.extend(windows::detect_alternative_targets(&targets[0]));
            targets
        } else if #[cfg(target_os = "linux")] {
            // Linux is a bit special, since the result from `guess_host_triple`
            // might be wrong about whether glibc or musl is used.
            linux::detect_targets(target).await
        }
    }
}

/// Figure out what the host target is using `rustc`.
/// If `rustc` is absent, then it would return `None`.
///
/// If environment variable `CARGO` is present, then
/// `$CARGO -vV` will be run instead.
///
/// Otherwise, it will run `rustc -vV` to detect target.
async fn get_target_from_rustc() -> Option<String> {
    let cmd = env::var_os("CARGO")
        .map(Cow::Owned)
        .unwrap_or_else(|| Cow::Borrowed(OsStr::new("rustc")));

    let Output { status, stdout, .. } = Command::new(cmd)
        .arg("-vV")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?
        .wait_with_output()
        .await
        .ok()?;

    if !status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&stdout);
    let target = stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))?;

    // The target triplets have the form of 'arch-vendor-system'.
    //
    // When building for Linux (e.g. the 'system' part is
    // 'linux-something'), replace the vendor with 'unknown'
    // so that mapping to rust standard targets happens correctly.
    //
    // For example, alpine set `rustc` host triple to
    // `x86_64-alpine-linux-musl`.
    //
    // Here we use splitn with n=4 since we just need to check
    // the third part to see if it equals to "linux" and verify
    // that we have at least three parts.
    let mut parts: Vec<&str> = target.splitn(4, '-').collect();
    if *parts.get(2)? == "linux" {
        parts[1] = "unknown";
    }
    Some(parts.join("-"))
}
