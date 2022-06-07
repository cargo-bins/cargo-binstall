use arrayvec::ArrayVec;
use std::io::{BufRead, Cursor};
use std::iter::IntoIterator;
use std::ops::Deref;
use std::process::Output;
use std::slice;
use tokio::process::Command;

/// Compiled target triple, used as default for binary fetching
pub const TARGET: &str = env!("TARGET");

#[derive(Debug, Clone)]
pub struct Targets(ArrayVec<Box<str>, 2>);

impl Targets {
    fn from_array<const LEN: usize>(arr: [Box<str>; LEN]) -> Self {
        let mut v = ArrayVec::new();

        for elem in arr {
            v.push(elem);
        }

        Self(v)
    }

    fn push(&mut self, s: Box<str>) {
        self.0.push(s)
    }
}

impl Deref for Targets {
    type Target = [Box<str>];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl IntoIterator for Targets {
    type Item = Box<str>;
    type IntoIter = arrayvec::IntoIter<Box<str>, 2>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Targets {
    type Item = &'a Box<str>;
    type IntoIter = slice::Iter<'a, Box<str>>;

    fn into_iter(self) -> Self::IntoIter {
        (&self.0).into_iter()
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
pub async fn detect_targets() -> Targets {
    if let Some(target) = get_target_from_rustc().await {
        let mut v = Targets::from_array([target]);

        #[cfg(target_os = "linux")]
        if v[0].contains("gnu") {
            v.push(v[0].replace("gnu", "musl").into_boxed_str());
        }

        #[cfg(target_os = "macos")]
        if &*v[0] == macos::AARCH64 {
            v.push(macos::X86.into());
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
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Targets::from_array([TARGET.into()])
        }
    }
}

/// Figure out what the host target is using `rustc`.
/// If `rustc` is absent, then it would return `None`.
async fn get_target_from_rustc() -> Option<Box<str>> {
    let Output { status, stdout, .. } = Command::new("rustc").arg("-vV").output().await.ok()?;
    if !status.success() {
        return None;
    }

    Cursor::new(stdout)
        .lines()
        .filter_map(|line| line.ok())
        .find_map(|line| {
            line.strip_prefix("host: ")
                .map(|host| host.to_owned().into_boxed_str())
        })
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Command, Output, Targets, TARGET};

    pub(super) async fn detect_targets_linux() -> Targets {
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
                    return Targets::from_array([create_target_str("musl", abi)]);
                };

            if libc_version == "gnu" {
                return Targets::from_array([
                    create_target_str("gnu", abi),
                    create_target_str("musl", abi),
                ]);
            }
        }

        // Fallback to using musl
        Targets::from_array([create_target_str("musl", abi)])
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
    use super::Targets;
    use guess_host_triple::guess_host_triple;

    pub(super) const AARCH64: &str = "aarch64-apple-darwin";
    pub(super) const X86: &str = "x86_64-apple-darwin";

    pub(super) fn detect_targets_macos() -> Targets {
        if guess_host_triple() == Some(AARCH64) {
            Targets::from_array([AARCH64.into(), X86.into()])
        } else {
            Targets::from_array([X86.into()])
        }
    }
}
