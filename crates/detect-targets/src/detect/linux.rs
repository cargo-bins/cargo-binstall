use std::{
    process::{Output, Stdio},
    str::from_utf8 as str_from_utf8,
};

use tokio::{process::Command, task};

pub(super) async fn detect_targets(target: String) -> Vec<String> {
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
        // guess_host_triple cannot detect whether the system is using glibc,
        // musl libc or other libc.
        //
        // On Alpine, you can use `apk add gcompat` to install glibc
        // and run glibc programs.
        //
        // As such, we need to launch the test ourselves.
        Libc::Gnu | Libc::Musl => {
            let cpu_arch = target
                .split_once('-')
                .expect("unwrap: target always has a - for cpu_arch")
                .0;

            let has_glibc = task::spawn({
                let glibc_path = format!("/lib/ld-linux-{cpu_arch}.so.1");
                async move { is_gnu_ld(&glibc_path).await }
            });

            [
                has_glibc
                    .await
                    .unwrap_or(false)
                    .then(|| format!("{cpu_arch}-unknown-linux-gnu{abi}")),
                Some(musl_fallback_target()),
            ]
        }
        Libc::Android | Libc::Unknown => [Some(target.clone()), Some(musl_fallback_target())],
    }
    .into_iter()
    .flatten()
    .collect()
}

async fn is_gnu_ld(cmd: &str) -> bool {
    get_ld_flavor(cmd).await == Some(Libc::Gnu)
}

async fn get_ld_flavor(cmd: &str) -> Option<Libc> {
    let Output {
        status,
        stdout,
        stderr,
    } = Command::new(cmd)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;

    const ALPINE_GCOMPAT: &str = r#"This is the gcompat ELF interpreter stub.
You are not meant to run this directly.
"#;

    let stdout = str_from_utf8(&stdout).ok()?;
    let stderr = str_from_utf8(&stderr).ok()?;

    if status.success() {
        // Executing glibc ldd or /lib/ld-linux-{cpu_arch}.so.1 will always
        // succeeds.
        stdout.contains("GLIBC").then_some(Libc::Gnu)
    } else if status.code() == Some(1) {
        // On Alpine, executing both the gcompat glibc and the ldd and
        // /lib/ld-musl-{cpu_arch}.so.1 will fail with exit status 1.
        if stdout == ALPINE_GCOMPAT {
            // Alpine's gcompat package will output ALPINE_GCOMPAT to stdout
            Some(Libc::Gnu)
        } else if stderr.contains("musl libc") {
            // Alpine/s ldd and musl dynlib will output to stderr
            Some(Libc::Musl)
        } else {
            None
        }
    } else {
        None
    }
}

#[derive(Eq, PartialEq)]
enum Libc {
    Gnu,
    Musl,
    Android,
    Unknown,
}
