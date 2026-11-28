use std::{
    ffi::OsStr,
    io,
    process::{Output, Stdio},
    str,
    time::Duration,
};

use tokio::{io::AsyncWriteExt, process::Command};
use tracing::debug;
use zeroize::{Zeroize, Zeroizing};

pub(super) async fn get() -> io::Result<Zeroizing<Box<str>>> {
    get_from("gh".as_ref(), "git".as_ref()).await
}

async fn get_from(gh: &OsStr, git: &OsStr) -> io::Result<Zeroizing<Box<str>>> {
    match Command::new(gh)
        .args(["auth", "token"])
        .stdout_with_optional_input(None)
        .await
    {
        Ok(token) if !token.is_empty() => return Ok(token),
        Ok(_) => debug!("`gh auth token` printed no token"),
        Err(err) => debug!(?err, "Failed to get a token from `gh auth token`"),
    }

    Command::new(git)
        .args(["credential", "fill"])
        // Never ask the user: no terminal prompt, no askpass program and no
        // Git Credential Manager window.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("GCM_INTERACTIVE", "never")
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
        // A credential helper can block on a dialog or a passphrase prompt.
        self.kill_on_drop(true)
            .stdout(Stdio::piped())
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

        let Output { status, stdout, .. } =
            tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
                .await
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("`{:?}` timed out", self.as_std()),
                    )
                })??;

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

#[cfg(all(test, unix))]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use super::*;

    fn script(dir: &Path, name: &str, body: &str) -> Box<OsStr> {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.into_os_string().into_boxed_os_str()
    }

    fn get_with(gh: &OsStr, git: &OsStr) -> io::Result<Zeroizing<Box<str>>> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(get_from(gh, git))
    }

    #[test]
    fn falls_back_to_git_credential_fill() {
        let dir = tempfile::tempdir().unwrap();
        // Prints the token only if git cannot prompt.
        let git = script(
            dir.path(),
            "git",
            r#"[ "$GIT_TERMINAL_PROMPT" = 0 ] && [ -z "$GIT_ASKPASS" ] && [ "$GCM_INTERACTIVE" = never ] || exit 1
cat >/dev/null
echo protocol=https
echo host=github.com
echo password=git-token"#,
        );

        for gh in [
            // gh is not installed.
            dir.path()
                .join("missing")
                .into_os_string()
                .into_boxed_os_str(),
            // gh is not logged in.
            script(dir.path(), "gh-fails", "exit 1"),
            script(dir.path(), "gh-empty", "exit 0"),
        ] {
            assert_eq!(&**get_with(&gh, &git).unwrap(), "git-token");
        }

        let gh = script(dir.path(), "gh", "echo gh-token");
        assert_eq!(&**get_with(&gh, &git).unwrap(), "gh-token");
    }

    #[test]
    fn no_token() {
        let dir = tempfile::tempdir().unwrap();
        let gh = script(dir.path(), "gh", "exit 1");
        for (name, body) in [
            ("git-fails", "exit 128"),
            ("git-no-password", "echo username=x"),
        ] {
            let git = script(dir.path(), name, body);
            assert!(get_with(&gh, &git).is_err());
        }
    }
}
