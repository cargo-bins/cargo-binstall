use std::{
    io,
    process::{Output, Stdio},
};

use compact_str::CompactString;
use tokio::process::Command;

pub(super) async fn get() -> io::Result<CompactString> {
    let Output { status, stdout, .. } = Command::new("gh")
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;

    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("process exited with `{status}`"),
        ));
    }

    // Use String here instead of CompactString here since
    // `CompactString::from_utf8` allocates if it's longer than 24B.
    let s = String::from_utf8(stdout).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid output, expected utf8: {err}"),
        )
    })?;

    Ok(s.trim().into())
}
