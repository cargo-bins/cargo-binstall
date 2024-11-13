use std::{
    io,
    process::{Output, Stdio},
    str,
};

use tokio::{io::AsyncWriteExt, process::Command};
use zeroize::Zeroizing;

pub(super) async fn get() -> io::Result<Zeroizing<Box<str>>> {
    // Prepare the input for the git credential fill command
    let input = "host=github.com\nprotocol=https";

    let Output { status, stdout, .. } = Command::new("git")
        .args(["credential", "fill"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output_async_with_stdin(input.as_bytes())
        .await?;

    let stdout = Zeroizing::new(stdout);

    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("process exited with `{status}`"),
        ));
    }

    // Assuming the password field is what's needed
    let output_str = str::from_utf8(&stdout).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid output, expected utf8: {err}"),
        )
    })?;

    // Extract the password from the output
    let password = output_str
        .lines()
        .find_map(|line| {
            if line.starts_with("password=") {
                Some(line.trim_start_matches("password=").to_owned())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Other,
                "Password not found in the credential output",
            )
        })?;

    Ok(Zeroizing::new(password.into()))
}

trait CommandExt {
    // Helper function to execute a command with input
    async fn output_async_with_stdin(&mut self, input: &[u8]) -> io::Result<Output>;
}

impl CommandExt for Command {
    async fn output_async_with_stdin(&mut self, input: &[u8]) -> io::Result<Output> {
        let mut child = self.spawn()?;

        child
            .stdin
            .take()
            .expect("Failed to open stdin")
            .write_all(input)
            .await?;

        child.wait_with_output().await
    }
}
