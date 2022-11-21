use crate::TARGET;
use guess_host_triple::guess_host_triple;
use std::iter;

const AARCH64: &str = "aarch64-apple-darwin";
const X86: &str = "x86_64-apple-darwin";
const UNIVERSAL: &str = "universal-apple-darwin";

pub(super) fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> {
    (target == AARCH64)
        .then(|| X86.to_string())
        .into_iter()
        .chain(iter::once(UNIVERSAL.to_string()))
}

pub(super) fn detect_targets_macos() -> Vec<String> {
    let mut targets = vec![guess_host_triple().unwrap_or(TARGET).to_string()];

    targets.extend(detect_alternative_targets(&targets[0]));

    targets
}
