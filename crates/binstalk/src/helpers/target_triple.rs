use std::{borrow::Cow, str::FromStr};

use compact_str::{CompactString, ToCompactString};
use target_lexicon::Triple;

use crate::{errors::BinstallError, helpers::is_universal_macos};

#[derive(Clone, Debug)]
pub struct TargetTriple {
    // TODO: Once https://github.com/bytecodealliance/target-lexicon/pull/90
    // lands, consider replacing use of CompactString with `Cow<'_, str>`.
    pub target_family: CompactString,
    pub target_arch: CompactString,
    pub target_libc: CompactString,
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
            target_family: triple.operating_system.to_compact_string(),
            target_arch: if is_universal_macos {
                "universal".to_compact_string()
            } else {
                triple.architecture.to_compact_string()
            },
            target_libc: triple.environment.to_compact_string(),
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
