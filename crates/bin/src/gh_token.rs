use std::{io, process};

use compact_str::CompactString;

pub(super) fn get() -> io::Result<CompactString> {
    let process::Output { status, stdout, .. } = process::Command::new("gh")
        .args(["auth", "token"])
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::null())
        .output()?;

    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("process exited with `{status}`"),
        ));
    }

    // Use String here instead of CompactString here since
    // `CompactString::from_utf8` allocates if it's longer than 24B.
    let s = String::from_utf8(stdout).map_err(|_err| {
        io::Error::new(io::ErrorKind::InvalidData, "Invalid output, expected utf8")
    })?;

    Ok(s.trim().into())
}
