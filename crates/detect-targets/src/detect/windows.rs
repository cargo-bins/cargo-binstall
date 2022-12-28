use crate::TARGET;
use guess_host_triple::guess_host_triple;

pub(super) fn detect_alternative_targets(target: &str) -> impl Iterator<Item = String> {
    // This rsplit will succeed on valid target triple, which we assume
    // is valid.
    let (prefix, abi) = target.rsplit_once('-').unwrap();

    // AFAIK only windows 10/11 supports arm and it requires > 4G of ram,
    // which makes running it on armv7 moot.
    let is_arm = prefix.starts_with("aarch64");

    // detect abi in ["gnu", "gnullvm", ...]
    let is_gnu_abi = abi != "msvc";

    let msvc_fallback = is_gnu_abi.then(|| format!("{prefix}-msvc"));
    let x86_64_fallback = is_arm.then(|| "x86_64-pc-windows-mscv".to_string());

    msvc_fallback.into_iter().chain(x86_64_fallback)
}

pub(super) fn detect_targets_windows() -> Vec<String> {
    let mut targets = vec![guess_host_triple().unwrap_or(TARGET).to_string()];

    targets.extend(detect_alternative_targets(&targets[0]));

    targets
}
