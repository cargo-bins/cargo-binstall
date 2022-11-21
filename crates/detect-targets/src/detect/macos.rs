use crate::TARGET;
use guess_host_triple::guess_host_triple;

const AARCH64: &str = "aarch64-apple-darwin";
const X86: &str = "x86_64-apple-darwin";
const UNIVERSAL: &str = "universal-apple-darwin";

pub(super) fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> {
    let is_aarch64 = target == AARCH64;
    let is_x86 = target == X86;

    is_aarch64
        .then(|| X86.to_string())
        .into_iter()
        .chain((is_aarch64 || is_x86).then(|| UNIVERSAL.to_string()))
}

pub(super) fn detect_targets_macos() -> Vec<String> {
    let mut targets = vec![guess_host_triple().unwrap_or(TARGET).to_string()];

    targets.extend(detect_alternative_targets(&targets[0]));

    targets
}
