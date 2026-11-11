//! Legacy glibc detection: probe a list of known dynamic loader paths
//! and parse `--version` banners.
//!
//! Only used when the exec-probe ([`crate::probe`]) is inconclusive,
//! i.e. the environment prevented running the probe executable at all
//! (noexec temporary directory, seccomp, LSM policy, ...). Unlike the
//! exec-probe this is heuristic: distros rebrand the version banner
//! (e.g. Gentoo), and loader paths vary.

use std::process::{Output, Stdio};

use tokio::{process::Command, task};
#[cfg(feature = "tracing")]
use tracing::debug;

pub(super) async fn has_glibc(cpu_arch: &str, abi: &str) -> bool {
    let handles: Vec<_> = {
        let cpu_arch_suffix = cpu_arch.replace('_', "-");
        let filename = format!("ld-linux-{cpu_arch_suffix}.so.2");
        let dirname = format!("{cpu_arch}-linux-gnu");

        let mut probe_paths = vec![
            format!("/lib/{filename}"),
            format!("/lib64/{filename}"),
            format!("/lib/{dirname}/{filename}"),
            format!("/lib64/{dirname}/{filename}"),
            format!("/usr/lib/{dirname}/{filename}"),
            format!("/usr/lib64/{dirname}/{filename}"),
            "/usr/lib64/libc.so.6".to_string(),
            format!("/usr/lib/{dirname}/libc.so.6"),
            format!("/usr/lib64/{dirname}/libc.so.6"),
            "/usr/lib64/libc.so".to_string(),
            format!("/usr/lib/{dirname}/libc.so"),
            format!("/usr/lib64/{dirname}/libc.so"),
        ];

        // TODO: Find out if other arm-based (32 bit) `target_arch`s use the same
        // generic "arm" arch in both /lib and /usr/lib prefixed paths to libc.so.6.
        // For now, only do this for armv7 target_arch because it has been empirically proven.
        // See: https://github.com/cargo-bins/cargo-binstall/issues/2386
        if cpu_arch == "armv7" {
            // note, `abi` is appended in this case
            let arm_dirname = format!("arm-linux-gnu{abi}");
            probe_paths.extend([
                format!("/lib/{arm_dirname}/libc.so.6"),
                format!("/usr/lib/{arm_dirname}/libc.so.6"),
            ]);
        }

        probe_paths
            .into_iter()
            .map(|p| AutoAbortHandle(tokio::spawn(is_gnu_ld(p))))
            .collect()
    };

    for mut handle in handles {
        if let Ok(true) = (&mut handle.0).await {
            return true;
        }
    }

    false
}

async fn is_gnu_ld(cmd: String) -> bool {
    get_ld_flavor(&cmd).await == Some(LdFlavor::Gnu)
}

async fn get_ld_flavor(cmd: &str) -> Option<LdFlavor> {
    let Output {
        status,
        stdout,
        stderr,
    } = match Command::new(cmd)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .await
    {
        Ok(output) => output,
        Err(_err) => {
            #[cfg(feature = "tracing")]
            debug!("Running `{cmd} --version`: err={_err:?}");
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);

    #[cfg(feature = "tracing")]
    debug!("`{cmd} --version`: status={status}, stdout='{stdout}', stderr='{stderr}'");

    const ALPINE_GCOMPAT: &str = r#"This is the gcompat ELF interpreter stub.
You are not meant to run this directly.
"#;

    if status.success() {
        // Executing glibc ldd or /lib/ld-linux-{cpu_arch}.so.1 will always
        // succeeds.
        (stdout.contains("GLIBC") || stdout.contains("GNU libc")).then_some(LdFlavor::Gnu)
    } else if status.code() == Some(1) {
        // On Alpine, executing both the gcompat glibc and the ldd and
        // /lib/ld-musl-{cpu_arch}.so.1 will fail with exit status 1.
        if stdout == ALPINE_GCOMPAT {
            // Alpine's gcompat package will output ALPINE_GCOMPAT to stdout
            Some(LdFlavor::Gnu)
        } else if stderr.contains("musl libc") {
            // Alpine/s ldd and musl dynlib will output to stderr
            Some(LdFlavor::Musl)
        } else {
            None
        }
    } else if status.code() == Some(127) {
        // On Ubuntu 20.04 (glibc 2.31), the `--version` flag is not supported
        // and it will exit with status 127.
        let status = Command::new(cmd)
            .arg("/bin/true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .ok()?;

        #[cfg(feature = "tracing")]
        debug!("`{cmd} --version`: status={status}");

        status.success().then_some(LdFlavor::Gnu)
    } else {
        None
    }
}

#[derive(Eq, PartialEq)]
enum LdFlavor {
    Gnu,
    Musl,
}

struct AutoAbortHandle<T>(task::JoinHandle<T>);

impl<T> Drop for AutoAbortHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
