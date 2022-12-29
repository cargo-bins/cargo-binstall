use crate::CowStr;

const AARCH64: &str = "aarch64-apple-darwin";
const X86: &str = "x86_64-apple-darwin";
const UNIVERSAL: &str = "universal-apple-darwin";

pub(super) fn detect_alternative_targets(target: &str) -> impl Iterator<Item = CowStr> {
    match target {
        AARCH64 => [
            Some(CowStr::borrowed(X86)),
            Some(CowStr::borrowed(UNIVERSAL)),
        ],
        X86 => [Some(CowStr::borrowed(UNIVERSAL)), None],
        _ => [None, None],
    }
    .into_iter()
    .flatten()
}
