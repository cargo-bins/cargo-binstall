use std::{
    process::{Output, Stdio},
    str,
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

            let cpu_arch_suffix = cpu_arch.replace('_', "-");

            let handles: Vec<_> = [
                format!("/lib/ld-linux-{cpu_arch_suffix}.so.2"),
                format!("/lib/{cpu_arch}-linux-gnu/ld-linux-{cpu_arch_suffix}.so.2"),
                format!("/usr/lib/{cpu_arch}-linux-gnu/ld-linux-{cpu_arch_suffix}.so.2"),
            ]
            .into_iter()
            .map(|p| AutoAbortHandle(tokio::spawn(is_gnu_ld(p))))
            .collect();

            let has_glibc = async move {
                for mut handle in handles {
                    if let Ok(true) = (&mut handle.0).await {
                        return true;
                    }
                }

                false
            }
            .await;

            [
                has_glibc.then(|| format!("{cpu_arch}-unknown-linux-gnu{abi}")),
                Some(musl_fallback_target()),
            ]
        }
        Libc::Android | Libc::Unknown => [Some(target.clone()), Some(musl_fallback_target())],
    }
    .into_iter()
    .flatten()
    .collect()
}

async fn is_gnu_ld(cmd: String) -> bool {
    get_ld_flavor(&cmd).await == Some(Libc::Gnu)
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

    if status.success() {
        // Executing glibc ldd or /lib/ld-linux-{cpu_arch}.so.1 will always
        // succeeds.
        String::from_utf8_lossy(&stdout)
            .contains("GLIBC")
            .then_some(Libc::Gnu)
    } else if status.code() == Some(1) {
        // On Alpine, executing both the gcompat glibc and the ldd and
        // /lib/ld-musl-{cpu_arch}.so.1 will fail with exit status 1.
        if str::from_utf8(&stdout).as_deref() == Ok(ALPINE_GCOMPAT) {
            // Alpine's gcompat package will output ALPINE_GCOMPAT to stdout
            Some(Libc::Gnu)
        } else if String::from_utf8_lossy(&stderr).contains("musl libc") {
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

struct AutoAbortHandle<T>(task::JoinHandle<T>);

impl<T> Drop for AutoAbortHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
