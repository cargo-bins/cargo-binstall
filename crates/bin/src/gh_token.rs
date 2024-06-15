use std::{
    io,
    process::{Output, Stdio},
    str,
};

use tokio::process::Command;
use zeroize::Zeroizing;

pub(super) async fn get() -> io::Result<Zeroizing<Box<str>>> {
    let Output { status, stdout, .. } = Command::new("gh")
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;

    let stdout = Zeroizing::new(stdout);

    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("process exited with `{status}`"),
        ));
    }

    let s = str::from_utf8(&stdout).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid output, expected utf8: {err}"),
        )
    })?;

    Ok(Zeroizing::new(s.trim().into()))
}
