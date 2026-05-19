use std::{borrow::Cow, ops::Deref, str::FromStr};

use binstalk_types::cargo_toml_binstall::{CfgOption, TargetTriple as TargetTripleInner};

use crate::errors::BinstallError;

#[derive(Clone, Debug)]
pub struct TargetTriple(TargetTripleInner);

impl Deref for TargetTriple {
    type Target = TargetTripleInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FromStr for TargetTriple {
    type Err = BinstallError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

impl leon::Values for TargetTriple {
    fn get_value<'s>(&'s self, key: &str) -> Option<Cow<'s, str>> {
        match key {
            // Intentional: `target-family` in the template refers to the OS,
            // not to the `rustc` target family.
            "target-family" => Some(self.os.into_str()),
            // `rustc`-style OS name, e.g. `macos` rather than `darwin`.
            "os-name" => Some(CfgOption::Os(self.os).value()),
            "target-arch" => Some(self.arch.into_str()),
            "target-libc" => Some(self.env.into_str()),
            "target-vendor" => Some(Cow::Borrowed(self.vendor.as_str())),

            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leon::Values;

    #[test]
    fn test_os_name_macos() {
        let triple: TargetTriple = "aarch64-apple-darwin".parse().unwrap();
        assert_eq!(triple.get_value("os-name").as_deref(), Some("macos"));
    }

    #[test]
    fn test_os_name_linux() {
        let triple: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();
        assert_eq!(triple.get_value("os-name").as_deref(), Some("linux"));
    }

    #[test]
    fn test_os_name_windows() {
        let triple: TargetTriple = "x86_64-pc-windows-msvc".parse().unwrap();
        assert_eq!(triple.get_value("os-name").as_deref(), Some("windows"));
    }

    #[test]
    fn test_existing_keys_unchanged() {
        let triple: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();
        assert_eq!(triple.get_value("target-arch").as_deref(), Some("x86_64"));
        assert_eq!(triple.get_value("target-libc").as_deref(), Some("gnu"));
        assert_eq!(triple.get_value("unknown-key"), None);
    }
}
