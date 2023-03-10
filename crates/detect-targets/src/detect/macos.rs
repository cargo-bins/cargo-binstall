use std::{io, process::Stdio};

use tokio::process::Command;

const AARCH64: &str = "aarch64-apple-darwin";
const X86: &str = "x86_64-apple-darwin";
const UNIVERSAL: &str = "universal-apple-darwin";

async fn is_x86_64_supported() -> io::Result<bool> {
    let exit_status = Command::new("arch")
        .args(["-arch", "x86_64", "/usr/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?
        .wait()
        .await?;

    Ok(exit_status.success())
}

pub(super) async fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> {
    match target {
        AARCH64 => [
            is_x86_64_supported().await.unwrap_or(false).then_some(X86),
            Some(UNIVERSAL),
        ],
        X86 => [Some(UNIVERSAL), None],
        _ => [None, None],
    }
    .into_iter()
    .flatten()
    .map(ToString::to_string)
}
