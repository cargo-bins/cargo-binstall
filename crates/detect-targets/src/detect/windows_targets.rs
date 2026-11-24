//! Which Windows targets a host can run, kept apart from the Windows API
//! calls in `windows.rs` so that it is unit tested on every platform.

/// <https://learn.microsoft.com/en-us/windows/arm/arm64ec>
const ARM64EC_TARGET: &str = "arm64ec-pc-windows-msvc";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Machine {
    X64,
    X86,
    Arm32,
    Arm64,
}

/// Targets that `target`'s host can also run, in the order of preference.
///
/// `is_supported` tells whether Windows can run code for a machine other
/// than `target`'s own.
pub(super) fn alternative_targets<F: Fn(Machine) -> bool>(
    target: &str,
    is_supported: F,
) -> impl Iterator<Item = String> + use<F> {
    let (prefix, abi) = target
        .rsplit_once('-')
        .expect("unwrap: target always has a -");

    let arch = prefix
        .split_once('-')
        .expect("unwrap: target always has at least two -")
        .0;

    let msvc_fallback_target = (abi != "msvc").then(|| format!("{prefix}-msvc"));

    // Arm64EC only has an msvc target.
    let gnu_fallback_targets = (abi == "msvc" && arch != "arm64ec")
        .then(|| [format!("{prefix}-gnu"), format!("{prefix}-gnullvm")])
        .into_iter()
        .flatten();

    let x64_supported = arch == "x86_64" || is_supported(Machine::X64);
    let arm64_supported = arch == "aarch64" || is_supported(Machine::Arm64);

    // Arm64EC binaries need both a native ARM64 CPU and the x64 emulation
    // that Windows 11 on ARM64 has, Windows 10 on ARM64 cannot run them.
    //
    // On a native ARM64 host they are preferred to x64 ones, since they
    // run natively.
    let arm64ec_supported = arch != "arm64ec" && arm64_supported && x64_supported;
    let arm64ec_fallback_target = |prefer_to_x64: bool| {
        (arm64ec_supported && prefer_to_x64 == (arch == "aarch64"))
            .then(|| ARM64EC_TARGET.to_string())
    };

    let x64_fallback_targets = (arch != "x86_64" && x64_supported)
        .then_some([
            "x86_64-pc-windows-msvc",
            "x86_64-pc-windows-gnu",
            "x86_64-pc-windows-gnullvm",
        ])
        .into_iter()
        .flatten()
        .map(ToString::to_string);

    let x86_fallback_targets = (arch != "x86" && is_supported(Machine::X86))
        .then_some([
            "i586-pc-windows-msvc",
            "i586-pc-windows-gnu",
            "i586-pc-windows-gnullvm",
            "i686-pc-windows-msvc",
            "i686-pc-windows-gnu",
            "i686-pc-windows-gnullvm",
        ])
        .into_iter()
        .flatten()
        .map(ToString::to_string);

    let arm32_fallback_targets = (arch != "thumbv7a" && is_supported(Machine::Arm32))
        .then_some([
            "thumbv7a-pc-windows-msvc",
            "thumbv7a-pc-windows-gnu",
            "thumbv7a-pc-windows-gnullvm",
        ])
        .into_iter()
        .flatten()
        .map(ToString::to_string);

    let arm64_fallback_targets = (arch != "aarch64" && arm64_supported)
        .then_some([
            "aarch64-pc-windows-msvc",
            "aarch64-pc-windows-gnu",
            "aarch64-pc-windows-gnullvm",
        ])
        .into_iter()
        .flatten()
        .map(ToString::to_string);

    msvc_fallback_target
        .into_iter()
        .chain(gnu_fallback_targets)
        .chain(arm64ec_fallback_target(true))
        .chain(x64_fallback_targets)
        .chain(x86_fallback_targets)
        .chain(arm32_fallback_targets)
        .chain(arm64_fallback_targets)
        .chain(arm64ec_fallback_target(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(target: &str, supported: &[Machine]) -> Vec<String> {
        alternative_targets(target, |machine| supported.contains(&machine)).collect()
    }

    const X64: [&str; 3] = [
        "x86_64-pc-windows-msvc",
        "x86_64-pc-windows-gnu",
        "x86_64-pc-windows-gnullvm",
    ];
    const X86: [&str; 6] = [
        "i586-pc-windows-msvc",
        "i586-pc-windows-gnu",
        "i586-pc-windows-gnullvm",
        "i686-pc-windows-msvc",
        "i686-pc-windows-gnu",
        "i686-pc-windows-gnullvm",
    ];
    const ARM64: [&str; 3] = [
        "aarch64-pc-windows-msvc",
        "aarch64-pc-windows-gnu",
        "aarch64-pc-windows-gnullvm",
    ];

    #[test]
    fn arm64ec_is_preferred_to_x64_on_windows_11_arm64() {
        let expected: Vec<_> = [
            "aarch64-pc-windows-gnu",
            "aarch64-pc-windows-gnullvm",
            ARM64EC_TARGET,
        ]
        .into_iter()
        .chain(X64)
        .chain(X86)
        .collect();
        assert_eq!(
            targets(
                "aarch64-pc-windows-msvc",
                &[Machine::X64, Machine::X86, Machine::Arm64]
            ),
            expected,
        );
    }

    /// Windows 10 on ARM64 has neither x64 emulation nor
    /// `GetMachineTypeAttributes`, so nothing is reported as supported.
    #[test]
    fn no_arm64ec_on_windows_10_arm64() {
        assert_eq!(
            targets("aarch64-pc-windows-msvc", &[]),
            ["aarch64-pc-windows-gnu", "aarch64-pc-windows-gnullvm"],
        );
    }

    /// e.g. an x64 `rustc` on ARM64: it stays first, then native ARM64.
    #[test]
    fn arm64ec_after_arm64_for_x64_on_arm64() {
        let expected: Vec<_> = ["x86_64-pc-windows-gnu", "x86_64-pc-windows-gnullvm"]
            .into_iter()
            .chain(X86)
            .chain(ARM64)
            .chain([ARM64EC_TARGET])
            .collect();
        assert_eq!(
            targets("x86_64-pc-windows-msvc", &[Machine::X86, Machine::Arm64]),
            expected,
        );
    }

    #[test]
    fn no_arm64ec_on_x64() {
        let expected: Vec<_> = ["x86_64-pc-windows-gnu", "x86_64-pc-windows-gnullvm"]
            .into_iter()
            .chain(X86)
            .collect();
        assert_eq!(targets("x86_64-pc-windows-msvc", &[Machine::X86]), expected);
    }

    #[test]
    fn arm64ec_has_no_gnu_targets() {
        let expected: Vec<_> = X64.into_iter().chain(ARM64).collect();
        assert_eq!(
            targets(ARM64EC_TARGET, &[Machine::X64, Machine::Arm64]),
            expected,
        );
    }
}
