use std::{fs::File, io::Write, process::Command};
#[cfg(unix)]
use std::{fs::Permissions, os::unix::fs::PermissionsExt};

use tempfile::tempdir;

use crate::errors::BinstallError;

const WASI_PROGRAM: &[u8] = include_bytes!("miniwasi.wasm");

/// Detect the ability to run WASI
///
/// This attempts to run a small embedded WASI program, and returns true if no errors happened.
/// Errors returned by the `Result` are I/O errors from the establishment of the context, not
/// errors from the run attempt.
///
/// On Linux, you can configure your system to run WASI programs using a binfmt directive. Under
/// systemd, write the below to `/etc/binfmt.d/wasi.conf`, with `/usr/bin/wasmtime` optionally
/// replaced with the path to your WASI runtime of choice:
///
/// ```plain
/// :wasi:M::\x00asm::/usr/bin/wasmtime:
/// ```
pub fn detect_wasi_runability() -> Result<bool, BinstallError> {
    let progdir = tempdir()?;
    let prog = progdir.path().join("miniwasi.wasm");

    {
        let mut progfile = File::create(&prog)?;
        progfile.write_all(WASI_PROGRAM)?;

        #[cfg(unix)]
        progfile.set_permissions(Permissions::from_mode(0o777))?;
    }

    match Command::new(prog).output() {
        Ok(out) => Ok(out.status.success() && out.stdout.is_empty() && out.stderr.is_empty()),
        Err(_) => Ok(false),
    }
}
