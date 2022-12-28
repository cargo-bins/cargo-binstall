pub(super) fn detect_alternative_targets(target: &str) -> Option<String> {
    let (prefix, abi) = target.rsplit_once('-')?;

    // detect abi in ["gnu", "gnullvm", ...]
    (abi != "msvc").then(|| format!("{prefix}-msvc"))
}
