use std::{io, process};

use compact_str::CompactString;
use tracing::warn;

fn get_inner() -> io::Result<CompactString> {
    let process::Output { status, stdout, .. } = process::Command::new("gh")
        .args(["auth", "token"])
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::null())
        .output()?;

    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to execute `gh auth token`, exit status `{status}`"),
        ));
    }

    // Use String here instead of CompactString here since
    // `CompactString::from_utf8` allocates if it's longer than 24B.
    let s = String::from_utf8(stdout).map_err(|_err| {
        io::Error::new(io::ErrorKind::InvalidData, "Invalid token: expected ASCII")
    })?;

    Ok(s.trim().into())
}

pub(super) fn get() -> Option<CompactString> {
    match get_inner() {
        Ok(token) => Some(token),
        Err(err) => {
            warn!(?err);
            None
        }
    }
}
