use std::{borrow::Cow, collections::BTreeMap, ops::Deref, str::FromStr};

use binstalk_types::cargo_toml_binstall::{CfgOption, TargetTriple as TargetTripleInner};
use compact_str::CompactString;

use crate::errors::BinstallError;

#[derive(Clone, Debug)]
pub struct TargetTriple {
    inner: TargetTripleInner,
    /// Replaces the architecture name in `{ target-arch }`, set from
    /// `target-arch-alias` in the crate's metadata.
    arch_alias: Option<CompactString>,
}

impl TargetTriple {
    /// Uses the entry for this target's architecture in `target_arch_alias`,
    /// if there is one, as the value of `{ target-arch }`.
    ///
    /// `cfg(target_arch = ...)` keeps using the architecture's own name.
    pub fn set_arch_alias(&mut self, target_arch_alias: &BTreeMap<CompactString, CompactString>) {
        self.arch_alias = target_arch_alias
            .get(self.inner.arch.into_str().as_ref())
            .cloned();
    }
}

impl Deref for TargetTriple {
    type Target = TargetTripleInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl FromStr for TargetTriple {
    type Err = BinstallError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            inner: s.parse()?,
            arch_alias: None,
        })
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
            "target-arch" => Some(match &self.arch_alias {
                Some(alias) => Cow::Borrowed(alias.as_str()),
                None => self.arch.into_str(),
            }),
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
    fn test_arch_alias() {
        let aliases = BTreeMap::from([("aarch64".into(), "arm64".into())]);

        let mut triple: TargetTriple = "aarch64-apple-darwin".parse().unwrap();
        triple.set_arch_alias(&aliases);
        assert_eq!(triple.get_value("target-arch").as_deref(), Some("arm64"));
        // The alias only applies to the template, not to `cfg` matching.
        assert!(
            triple
                .cfgs()
                .iter()
                .any(|cfg| cfg.to_string() == r#"target_arch = "aarch64""#)
        );

        // Architectures without an entry keep their own name.
        let mut triple: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();
        triple.set_arch_alias(&aliases);
        assert_eq!(triple.get_value("target-arch").as_deref(), Some("x86_64"));
    }

    #[test]
    fn test_existing_keys_unchanged() {
        let triple: TargetTriple = "x86_64-unknown-linux-gnu".parse().unwrap();
        assert_eq!(triple.get_value("target-arch").as_deref(), Some("x86_64"));
        assert_eq!(triple.get_value("target-libc").as_deref(), Some("gnu"));
        assert_eq!(triple.get_value("unknown-key"), None);
    }
}
