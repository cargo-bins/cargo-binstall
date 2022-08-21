use crate::TARGET;
use guess_host_triple::guess_host_triple;

pub(super) fn detect_alternative_targets(target: &str) -> Option<String> {
    let (prefix, abi) = target.rsplit_once('-')?;

    // detect abi in ["gnu", "gnullvm", ...]
    (abi != "msvc").then(|| format!("{prefix}-msvc"))
}

pub(super) fn detect_targets_windows() -> Vec<String> {
    let mut targets = vec![guess_host_triple().unwrap_or(TARGET).to_string()];

    targets.extend(detect_alternative_targets(&targets[0]));

    targets
}
