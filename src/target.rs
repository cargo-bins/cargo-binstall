use std::io::{BufRead, Cursor};
use std::process::Output;
use std::sync::Arc;

use tokio::process::Command;
use tokio::sync::OnceCell;

/// Compiled target triple, used as default for binary fetching
pub const TARGET: &str = env!("TARGET");

#[derive(Debug)]
enum DesiredTargetsInner {
    AutoDetect(Arc<OnceCell<Vec<String>>>),
    Initialized(Vec<String>),
}

#[derive(Debug)]
pub struct DesiredTargets(DesiredTargetsInner);

impl DesiredTargets {
    fn initialized(targets: Vec<String>) -> Self {
        Self(DesiredTargetsInner::Initialized(targets))
    }

    fn auto_detect() -> Self {
        let arc = Arc::new(OnceCell::new());

        let once_cell = arc.clone();
        tokio::spawn(async move {
            once_cell.get_or_init(detect_targets).await;
        });

        Self(DesiredTargetsInner::AutoDetect(arc))
    }

    pub async fn get(&self) -> &[String] {
        use DesiredTargetsInner::*;

        match &self.0 {
            Initialized(targets) => targets,

            // This will mostly just wait for the spawned task,
            // on rare occausion though, it will poll the future
            // returned by `detect_targets`.
            AutoDetect(once_cell) => once_cell.get_or_init(detect_targets).await,
        }
    }
}

/// If opts_targets is `Some`, then it will be used.
/// Otherwise, call `detect_targets` using `tokio::spawn` to detect targets.
///
/// Since `detect_targets` internally spawns a process and wait for it,
/// it's pretty costy.
///
/// Calling it through `tokio::spawn` would enable other tasks, such as
/// fetching the crate tarballs, to be executed concurrently.
pub fn get_desired_targets(opts_targets: &Option<String>) -> DesiredTargets {
    if let Some(targets) = opts_targets.as_ref() {
        DesiredTargets::initialized(targets.split(',').map(|t| t.to_string()).collect())
    } else {
        DesiredTargets::auto_detect()
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
        if v[0].contains("gnu") {
            v.push(v[0].replace("gnu", "msvc"));
        }

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

#[cfg(target_os = "linux")]
mod linux {
    use super::{Command, Output, TARGET};

    pub(super) async fn detect_targets_linux() -> Vec<String> {
        let abi = parse_abi();

        if let Ok(Output {
            status: _,
            stdout,
            stderr,
        }) = Command::new("ldd").arg("--version").output().await
        {
            let libc_version =
                if let Some(libc_version) = parse_libc_version_from_ldd_output(&stdout) {
                    libc_version
                } else if let Some(libc_version) = parse_libc_version_from_ldd_output(&stderr) {
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

    fn parse_abi() -> &'static str {
        let last = TARGET.rsplit_once('-').unwrap().1;

        if let Some(libc_version) = last.strip_prefix("musl") {
            libc_version
        } else if let Some(libc_version) = last.strip_prefix("gnu") {
            libc_version
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
}

#[cfg(target_os = "macos")]
mod macos {
    use guess_host_triple::guess_host_triple;

    pub(super) const AARCH64: &str = "aarch64-apple-darwin";
    pub(super) const X86: &str = "x86_64-apple-darwin";

    pub(super) fn detect_targets_macos() -> Vec<String> {
        if guess_host_triple() == Some(AARCH64) {
            vec![AARCH64.into(), X86.into()]
        } else {
            vec![X86.into()]
        }
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::TARGET;
    use guess_host_triple::guess_host_triple;

    pub(super) fn detect_targets_windows() -> Vec<String> {
        let mut targets = vec![guess_host_triple().unwrap_or(TARGET).to_string()];

        if targets[0].contains("gnu") {
            targets.push(targets[0].replace("gnu", "msvc"));
        }

        targets
    }
}
