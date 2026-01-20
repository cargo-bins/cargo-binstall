use std::{borrow::Cow, fmt::Display, str::FromStr};

use cargo_platform::{Cfg, Ident};
use target_lexicon::{Architecture, Environment, OperatingSystem, Triple, Vendor};

pub use target_lexicon::ParseError as TargetTripleParseError;

/// A representation of a `rustc` target triple. This is related to,
/// but different than, the LLVM target triple.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetTriple {
    pub os: OperatingSystem,
    pub arch: ArchOr,
    pub env: Environment,
    pub vendor: Vendor,
    pub family: Option<Family>,
}

impl TargetTriple {
    /// Returns a list of all the `cfg(...)` options that apply to this triple.
    pub fn cfgs(&self) -> Vec<Cfg> {
        let mut options = vec![
            CfgOption::Os(self.os),
            CfgOption::Arch(self.arch),
            CfgOption::Env(self.env),
            CfgOption::Vendor(&self.vendor),
        ];
        if let Some(family) = self.family {
            options.push(CfgOption::Family(family));
        }
        options.into_iter().flat_map(CfgOption::into_cfgs).collect()
    }
}

impl FromStr for TargetTriple {
    type Err = TargetTripleParseError;

    fn from_str(mut s: &str) -> Result<Self, Self::Err> {
        let is_universal_macos = is_universal_macos(s);

        if is_universal_macos {
            s = "x86_64-apple-darwin";
        }

        let triple = Triple::from_str(s)?;
        Ok(Self {
            os: triple.operating_system,
            arch: if is_universal_macos {
                ArchOr::Universal
            } else {
                ArchOr::Arch(triple.architecture)
            },
            env: triple.environment,
            vendor: triple.vendor,
            family: Family::from_os(triple.operating_system),
        })
    }
}

/// A `cfg(...)` option.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CfgOption<'a> {
    Os(OperatingSystem),
    Arch(ArchOr),
    Env(Environment),
    Vendor(&'a Vendor),
    Family(Family),
}

impl<'a> CfgOption<'a> {
    pub fn key(&self) -> &'static str {
        match self {
            Self::Os(_) => "target_os",
            Self::Arch(_) => "target_arch",
            Self::Env(_) => "target_env",
            Self::Vendor(_) => "target_vendor",
            Self::Family(_) => "target_family",
        }
    }

    pub fn value(&self) -> Cow<'a, str> {
        match self {
            Self::Os(OperatingSystem::Darwin(_) | OperatingSystem::MacOSX(_)) => {
                // `rustc` uses `macos` for both; match its behavior.
                Cow::Borrowed("macos")
            }
            Self::Os(os) => os.into_str(),
            Self::Arch(arch) => arch.into_str(),
            Self::Env(env) => env.into_str(),
            Self::Vendor(vendor) => vendor.as_str().into(),
            Self::Family(family) => family.to_string().into(),
        }
    }

    pub fn into_cfgs(self) -> Vec<Cfg> {
        let pair = Cfg::KeyPair(
            Ident {
                name: self.key().to_owned(),
                raw: false,
            },
            self.value().into_owned(),
        );
        match self {
            // For `cfg(target_family = unix | windows)`, also include
            // `cfg(unix | windows)`, matching `rustc`.
            Self::Family(family @ (Family::Unix | Family::Windows)) => vec![
                Cfg::Name(Ident {
                    name: family.to_string(),
                    raw: false,
                }),
                pair,
            ],
            // For other configuration options, including other target families,
            // just set `cfg(name = value)`.
            _ => vec![pair],
        }
    }
}

/// Either an [`Architecture`] from an LLVM target triple, or "universal",
/// which is an Apple multi-architecture binary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchOr {
    Arch(Architecture),
    Universal,
}

impl ArchOr {
    pub fn into_str(self) -> Cow<'static, str> {
        match self {
            Self::Arch(arch) => arch.into_str(),
            Self::Universal => Cow::Borrowed("universal"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Family {
    Unix,
    Windows,
}

impl Family {
    /// Converts an [`OperatingSystem`] from an LLVM target triple into
    /// a `rustc` target family.
    pub fn from_os(os: OperatingSystem) -> Option<Self> {
        // `rustc +nightly -Z unstable-options --print all-target-specs-json`
        // prints the mapping of each LLVM target triple to `target-family`.
        match os {
            OperatingSystem::Linux
            | OperatingSystem::Darwin(_)
            | OperatingSystem::MacOSX(_)
            | OperatingSystem::IOS(_)
            | OperatingSystem::Freebsd
            | OperatingSystem::Dragonfly
            | OperatingSystem::Openbsd
            | OperatingSystem::Netbsd
            | OperatingSystem::Solaris
            | OperatingSystem::Illumos
            | OperatingSystem::Fuchsia
            | OperatingSystem::Redox
            | OperatingSystem::Haiku
            | OperatingSystem::Aix
            | OperatingSystem::Cygwin
            | OperatingSystem::Emscripten
            | OperatingSystem::Espidf
            | OperatingSystem::Hurd
            | OperatingSystem::L4re
            | OperatingSystem::TvOS(_)
            | OperatingSystem::VisionOS(_)
            | OperatingSystem::VxWorks
            | OperatingSystem::WatchOS(_)
            | OperatingSystem::XROS(_) => Some(Self::Unix),
            OperatingSystem::Windows => Some(Self::Windows),
            _ => None,
        }
    }
}

impl Display for Family {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unix => "unix",
            Self::Windows => "windows",
        })
    }
}

fn is_universal_macos(target: &str) -> bool {
    ["universal-apple-darwin", "universal2-apple-darwin"].contains(&target)
}

#[cfg(test)]
mod tests {
    use super::*;

    use target_lexicon::Aarch64Architecture;

    #[test]
    fn test_parse_linux_target() {
        let triple: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();

        assert_eq!(triple.os, OperatingSystem::Linux);
        assert_eq!(triple.arch, ArchOr::Arch(Architecture::X86_64));
        assert_eq!(triple.env, Environment::Gnu);
        assert_eq!(triple.vendor, Vendor::Unknown);
        assert_eq!(triple.family, Some(Family::Unix));
    }

    #[test]
    fn test_parse_linux_musl_target() {
        let triple: TargetTriple = "aarch64-unknown-linux-musl".parse().unwrap();

        assert_eq!(triple.os, OperatingSystem::Linux);
        assert_eq!(
            triple.arch,
            ArchOr::Arch(Architecture::Aarch64(Aarch64Architecture::Aarch64)),
        );
        assert_eq!(triple.env, Environment::Musl);
        assert_eq!(triple.vendor, Vendor::Unknown);
        assert_eq!(triple.family, Some(Family::Unix));
    }

    #[test]
    fn test_parse_macos_target() {
        let triple: TargetTriple = "x86_64-apple-darwin".parse().unwrap();

        assert!(matches!(triple.os, OperatingSystem::Darwin(_)));
        assert_eq!(triple.arch, ArchOr::Arch(Architecture::X86_64));
        assert_eq!(triple.env, Environment::Unknown);
        assert_eq!(triple.vendor, Vendor::Apple);
        assert_eq!(triple.family, Some(Family::Unix));
    }

    #[test]
    fn test_parse_macos_aarch64_target() {
        let triple: TargetTriple = "aarch64-apple-darwin".parse().unwrap();

        assert!(matches!(triple.os, OperatingSystem::Darwin(_)));
        assert_eq!(
            triple.arch,
            ArchOr::Arch(Architecture::Aarch64(Aarch64Architecture::Aarch64))
        );
        assert_eq!(triple.vendor, Vendor::Apple);
        assert_eq!(triple.family, Some(Family::Unix));
    }

    #[test]
    fn test_parse_universal_apple_darwin() {
        let triple: TargetTriple = "universal-apple-darwin".parse().unwrap();

        assert!(matches!(triple.os, OperatingSystem::Darwin(_)));
        assert_eq!(triple.arch, ArchOr::Universal);
        assert_eq!(triple.vendor, Vendor::Apple);
        assert_eq!(triple.family, Some(Family::Unix));
    }

    #[test]
    fn test_parse_universal2_apple_darwin() {
        let triple: TargetTriple = "universal2-apple-darwin".parse().unwrap();

        assert!(matches!(triple.os, OperatingSystem::Darwin(_)));
        assert_eq!(triple.arch, ArchOr::Universal);
        assert_eq!(triple.vendor, Vendor::Apple);
        assert_eq!(triple.family, Some(Family::Unix));
    }

    #[test]
    fn test_parse_windows_target() {
        let triple: TargetTriple = "x86_64-pc-windows-msvc".parse().unwrap();

        assert_eq!(triple.os, OperatingSystem::Windows);
        assert_eq!(triple.arch, ArchOr::Arch(Architecture::X86_64));
        assert_eq!(triple.env, Environment::Msvc);
        assert_eq!(triple.vendor, Vendor::Pc);
        assert_eq!(triple.family, Some(Family::Windows));
    }

    #[test]
    fn test_parse_windows_gnu_target() {
        let triple: TargetTriple = "x86_64-pc-windows-gnu".parse().unwrap();

        assert_eq!(triple.os, OperatingSystem::Windows);
        assert_eq!(triple.arch, ArchOr::Arch(Architecture::X86_64));
        assert_eq!(triple.env, Environment::Gnu);
        assert_eq!(triple.vendor, Vendor::Pc);
        assert_eq!(triple.family, Some(Family::Windows));
    }

    #[test]
    fn test_parse_freebsd_target() {
        let triple: TargetTriple = "x86_64-unknown-freebsd".parse().unwrap();

        assert_eq!(triple.os, OperatingSystem::Freebsd);
        assert_eq!(triple.arch, ArchOr::Arch(Architecture::X86_64));
        assert_eq!(triple.family, Some(Family::Unix));
    }

    #[test]
    fn test_parse_invalid_target() {
        let result: Result<TargetTriple, _> = "bad-target-triple-foo".parse();
        assert!(result.is_err());
    }

    #[test]
    fn test_cfg_option_key() {
        assert_eq!(CfgOption::Os(OperatingSystem::Linux).key(), "target_os");
        assert_eq!(
            CfgOption::Arch(ArchOr::Arch(Architecture::X86_64)).key(),
            "target_arch"
        );
        assert_eq!(CfgOption::Env(Environment::Gnu).key(), "target_env");
        assert_eq!(CfgOption::Vendor(&Vendor::Unknown).key(), "target_vendor");
        assert_eq!(CfgOption::Family(Family::Unix).key(), "target_family");
    }

    #[test]
    fn test_cfg_option_value() {
        assert_eq!(CfgOption::Os(OperatingSystem::Linux).value(), "linux");
        assert_eq!(
            CfgOption::Arch(ArchOr::Arch(Architecture::X86_64)).value(),
            "x86_64"
        );
        assert_eq!(CfgOption::Arch(ArchOr::Universal).value(), "universal");
        assert_eq!(CfgOption::Env(Environment::Gnu).value(), "gnu");
        assert_eq!(CfgOption::Env(Environment::Msvc).value(), "msvc");
        assert_eq!(CfgOption::Vendor(&Vendor::Unknown).value(), "unknown");
        assert_eq!(CfgOption::Vendor(&Vendor::Apple).value(), "apple");
        assert_eq!(CfgOption::Family(Family::Unix).value(), "unix");
        assert_eq!(CfgOption::Family(Family::Windows).value(), "windows");
    }

    #[test]
    fn test_cfg_option_into_cfgs_os() {
        let cfgs = CfgOption::Os(OperatingSystem::Linux).into_cfgs();

        assert_eq!(cfgs.len(), 1);
        assert!(
            matches!(&cfgs[0], Cfg::KeyPair(ident, value) if ident.name == "target_os" && value == "linux")
        );
    }

    #[test]
    fn test_cfg_option_into_cfgs_family_unix() {
        let cfgs = CfgOption::Family(Family::Unix).into_cfgs();

        // `unix` should produce both `cfg(unix)` and
        // `cfg(target_family = "unix")`.
        assert_eq!(cfgs.len(), 2);
        assert!(matches!(&cfgs[0], Cfg::Name(ident) if ident.name == "unix"));
        assert!(
            matches!(&cfgs[1], Cfg::KeyPair(ident, value) if ident.name == "target_family" && value == "unix")
        );
    }

    #[test]
    fn test_cfg_option_into_cfgs_family_windows() {
        let cfgs = CfgOption::Family(Family::Windows).into_cfgs();

        // `windows` should produce both `cfg(windows)` and
        // `cfg(target_family = "windows")`.
        assert_eq!(cfgs.len(), 2);
        assert!(matches!(&cfgs[0], Cfg::Name(ident) if ident.name == "windows"));
        assert!(
            matches!(&cfgs[1], Cfg::KeyPair(ident, value) if ident.name == "target_family" && value == "windows")
        );
    }

    #[test]
    fn test_target_triple_cfgs_linux() {
        let triple: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();
        let cfgs = triple.cfgs();

        assert_eq!(cfgs.len(), 6);
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_os" && value == "linux")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_arch" && value == "x86_64")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_env" && value == "gnu")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_vendor" && value == "unknown")));
        assert!(cfgs
            .iter()
            .any(|c| matches!(c, Cfg::Name(ident) if ident.name == "unix")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_family" && value == "unix")));
    }

    #[test]
    fn test_target_triple_cfgs_windows() {
        let triple: TargetTriple = "x86_64-pc-windows-msvc".parse().unwrap();
        let cfgs = triple.cfgs();

        assert_eq!(cfgs.len(), 6);
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_os" && value == "windows")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_arch" && value == "x86_64")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_env" && value == "msvc")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_vendor" && value == "pc")));
        assert!(cfgs
            .iter()
            .any(|c| matches!(c, Cfg::Name(ident) if ident.name == "windows")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_family" && value == "windows")));
    }

    #[test]
    fn test_target_triple_cfgs_universal_macos() {
        let triple: TargetTriple = "universal-apple-darwin".parse().unwrap();
        let cfgs = triple.cfgs();

        assert_eq!(cfgs.len(), 6);
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_os" && value == "macos")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_arch" && value == "universal")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_env" && value == "unknown")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_vendor" && value == "apple")));
        assert!(cfgs
            .iter()
            .any(|c| matches!(c, Cfg::Name(ident) if ident.name == "unix")));
        assert!(cfgs.iter().any(|c| matches!(c, Cfg::KeyPair(ident, value) if ident.name == "target_family" && value == "unix")));
    }
}
