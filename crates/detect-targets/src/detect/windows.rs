use crate::CowStr;

pub(super) fn detect_alternative_targets(target: &str) -> Option<CowStr> {
    let (prefix, abi) = target.rsplit_once('-')?;

    // detect abi in ["gnu", "gnullvm", ...]
    (abi != "msvc").then(|| CowStr::owned(format!("{prefix}-msvc")))
}
