use std::{
    borrow::Cow,
    env,
    ffi::OsStr,
    io::{BufRead, Cursor},
    process::{Output, Stdio},
};

use cfg_if::cfg_if;
use tokio::process::Command;

use crate::TARGET;

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
    #[cfg(target_os = "linux")]
    {
        if let Some(target) = get_target_from_rustc().await {
            let mut targets = vec![target];

            if targets[0].contains("gnu") {
                targets.push(targets[0].replace("gnu", "musl"));
            }

            targets
        } else {
            linux::detect_targets_linux().await
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let target = get_target_from_rustc().await.unwrap_or_else(|| {
            guess_host_triple::guess_host_triple()
                .unwrap_or(TARGET)
                .to_string()
        });

        let mut targets = vec![target];

        cfg_if! {
            if #[cfg(target_os = "macos")] {
                targets.extend(macos::detect_alternative_targets(&targets[0]));
            } else if #[cfg(target_os = "windows")] {
                targets.extend(windows::detect_alternative_targets(&targets[0]));
            }
        }

        targets
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

    Cursor::new(stdout)
        .lines()
        .filter_map(|line| line.ok())
        .find_map(|line| line.strip_prefix("host: ").map(|host| host.to_owned()))
}
