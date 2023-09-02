use std::{
    fs,
    future::Future,
    path::Path,
    process::{Output, Stdio},
};

use tokio::{process::Command, task};

pub(super) async fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> {
    let (prefix, postfix) = target
        .rsplit_once('-')
        .expect("unwrap: target always has a -");

    let cpu_arch = target
        .split_once('-')
        .expect("unwrap: target always has a - for cpu_arch")
        .0;

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
        // guess_host_triple cannot detect whether the system is using glibc,
        // musl libc or other libc.
        //
        // As such, we need to launch the test ourselves.
        Libc::Gnu => {
            let has_glibc = spawn_has_glibc_task(cpu_arch);

            let distro_if_has_non_std_glibc = if is_gnu_ld("/usr/bin/ldd").await {
                get_distro_name().await
            } else {
                None
            };

            [
                has_glibc.await.then(|| target.to_string()),
                distro_if_has_non_std_glibc
                    .map(|distro_name| format!("{cpu_arch}-{distro_name}-linux-gnu{abi}")),
                Some(musl_fallback_target()),
            ]
        }
        Libc::Android | Libc::Unknown => {
            [Some(target.to_string()), Some(musl_fallback_target()), None]
        }

        Libc::Musl => {
            let has_glibc = spawn_has_glibc_task(cpu_arch);

            let distro_if_has_musl_dynlib = if get_ld_flavor(&format!(
                "/lib/ld-musl-{cpu_arch}.so.1"
            ))
            .await
                == Some(Libc::Musl)
            {
                get_distro_name().await
            } else {
                None
            };

            [
                // On Alpine, you can use `apk add gcompat` to install glibc
                // and run glibc programs.
                has_glibc
                    .await
                    .then(|| format!("{cpu_arch}-unknown-linux-gnu{abi}")),
                // Fallback for Linux flavors like Alpine, which has a musl dyn libc
                distro_if_has_musl_dynlib
                    .map(|distro_name| format!("{cpu_arch}-{distro_name}-linux-musl{abi}")),
                Some(musl_fallback_target()),
            ]
        }
    }
    .into_iter()
    .flatten()
}

fn spawn_has_glibc_task(cpu_arch: &str) -> impl Future<Output = bool> + Send + Sync + 'static {
    let glibc_path = format!("/lib/ld-linux-{cpu_arch}.so.1");
    async move {
        task::spawn(async move { is_gnu_ld(&glibc_path).await })
            .await
            .unwrap_or(false)
    }
}

async fn is_gnu_ld(cmd: &str) -> bool {
    get_ld_flavor(cmd).await == Some(Libc::Gnu)
}

async fn get_ld_flavor(cmd: &str) -> Option<Libc> {
    Command::new(cmd)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()
        .and_then(|Output { stdout, stderr, .. }| {
            Libc::parse(&stdout).or_else(|| Libc::parse(&stderr))
        })
}

#[derive(Eq, PartialEq)]
enum Libc {
    Gnu,
    Musl,
    Android,
    Unknown,
}

impl Libc {
    fn parse(output: &[u8]) -> Option<Self> {
        let s = String::from_utf8_lossy(output);
        if s.contains("musl libc") {
            Some(Self::Musl)
        } else if s.contains("GLIBC") {
            Some(Self::Gnu)
        } else {
            None
        }
    }
}

async fn get_distro_name() -> Option<String> {
    task::spawn_blocking(get_distro_name_blocking)
        .await
        .ok()
        .flatten()
}

fn get_distro_name_blocking() -> Option<String> {
    match fs::read_to_string("/etc/os-release") {
        Ok(os_release) => os_release
            .lines()
            .find_map(|line| line.strip_prefix("ID=\"")?.strip_suffix('"'))
            .map(ToString::to_string),
        Err(_) => (Path::new("/etc/nix/nix.conf").is_file()
            && ["/nix/store", "/nix/var/profiles"]
                .into_iter()
                .map(Path::new)
                .all(Path::is_dir))
        .then_some("nixos".to_string()),
    }
}
