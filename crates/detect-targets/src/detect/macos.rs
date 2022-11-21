use crate::TARGET;
use guess_host_triple::guess_host_triple;

const AARCH64: &str = "aarch64-apple-darwin";
const X86: &str = "x86_64-apple-darwin";
const UNIVERSAL: &str = "universal-apple-darwin";

pub(super) fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> {
    match target {
        AARCH64 => [Some(X86), Some(UNIVERSAL)],
        X86 => [Some(UNIVERSAL), None],
        _ => [None, None],
    }
    .into_iter()
    .flatten()
    .map(ToString::to_string)
}

pub(super) fn detect_targets_macos() -> Vec<String> {
    let mut targets = vec![guess_host_triple().unwrap_or(TARGET).to_string()];

    targets.extend(detect_alternative_targets(&targets[0]));

    targets
}
