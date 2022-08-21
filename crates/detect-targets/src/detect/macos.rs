use crate::TARGET;
use guess_host_triple::guess_host_triple;

pub(super) const AARCH64: &str = "aarch64-apple-darwin";
pub(super) const X86: &str = "x86_64-apple-darwin";

pub(super) fn detect_targets_macos() -> Vec<String> {
    let mut targets = vec![guess_host_triple().unwrap_or(TARGET).to_string()];

    if targets[0] == AARCH64 {
        targets.push(X86.into());
    }

    targets
}
