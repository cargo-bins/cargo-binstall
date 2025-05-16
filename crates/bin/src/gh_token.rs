use std::{
    io,
    process::{Output, Stdio},
    str,
};

use tokio::{io::AsyncWriteExt, process::Command};
use zeroize::{Zeroize, Zeroizing};

pub(super) async fn get() -> io::Result<Zeroizing<Box<str>>> {
    let output = Command::new("gh")
        .args(["auth", "token"])
        .stdout_with_optional_input(None)
        .await?;

    if !output.is_empty() {
        return Ok(output);
    }

    Command::new("git")
        .args(["credential", "fill"])
        .stdout_with_optional_input(Some("host=github.com\nprotocol=https".as_bytes()))
        .await?
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("password=")
                .map(|token| Zeroizing::new(token.into()))
        })
        .ok_or_else(|| io::Error::other("Password not found in `git credential fill` output"))
}

trait CommandExt {
    // Helper function to execute a command, optionally with input
    async fn stdout_with_optional_input(
        &mut self,
        input: Option<&[u8]>,
    ) -> io::Result<Zeroizing<Box<str>>>;
}

impl CommandExt for Command {
    async fn stdout_with_optional_input(
        &mut self,
        input: Option<&[u8]>,
    ) -> io::Result<Zeroizing<Box<str>>> {
        self.stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });

        let mut child = self.spawn()?;

        if let Some(input) = input {
            child.stdin.take().unwrap().write_all(input).await?;
        }

        let Output { status, stdout, .. } = child.wait_with_output().await?;

        if status.success() {
            let s = String::from_utf8(stdout).map_err(|err| {
                let msg = format!(
                    "Invalid output for `{:?}`, expected utf8: {err}",
                    self.as_std()
                );

                zeroize_and_drop(err.into_bytes());

                io::Error::new(io::ErrorKind::InvalidData, msg)
            })?;

            let trimmed = s.trim();

            Ok(if trimmed.len() == s.len() {
                Zeroizing::new(s.into_boxed_str())
            } else {
                Zeroizing::new(trimmed.into())
            })
        } else {
            zeroize_and_drop(stdout);

            Err(io::Error::other(format!(
                "`{:?}` process exited with `{status}`",
                self.as_std()
            )))
        }
    }
}

fn zeroize_and_drop(mut bytes: Vec<u8>) {
    bytes.zeroize();
}
