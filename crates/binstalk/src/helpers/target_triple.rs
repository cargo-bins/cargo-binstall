use std::{borrow::Cow, str::FromStr};

use compact_str::{CompactString, ToCompactString};
use target_lexicon::Triple;

use crate::{errors::BinstallError, helpers::is_universal_macos};

#[derive(Clone, Debug)]
pub struct TargetTriple {
    pub target_family: Cow<'static, str>,
    pub target_arch: Cow<'static, str>,
    pub target_libc: Cow<'static, str>,
    pub target_vendor: CompactString,
}

impl FromStr for TargetTriple {
    type Err = BinstallError;

    fn from_str(mut s: &str) -> Result<Self, Self::Err> {
        let is_universal_macos = is_universal_macos(s);

        if is_universal_macos {
            s = "x86_64-apple-darwin";
        }

        let triple = Triple::from_str(s)?;

        Ok(Self {
            target_family: triple.operating_system.into_str(),
            target_arch: if is_universal_macos {
                Cow::Borrowed("universal")
            } else {
                triple.architecture.into_str()
            },
            target_libc: triple.environment.into_str(),
            target_vendor: triple.vendor.to_compact_string(),
        })
    }
}

impl leon::Values for TargetTriple {
    fn get_value<'s>(&'s self, key: &str) -> Option<Cow<'s, str>> {
        match key {
            "target-family" => Some(Cow::Borrowed(&self.target_family)),
            "target-arch" => Some(Cow::Borrowed(&self.target_arch)),
            "target-libc" => Some(Cow::Borrowed(&self.target_libc)),
            "target-vendor" => Some(Cow::Borrowed(&self.target_vendor)),

            _ => None,
        }
    }
}
