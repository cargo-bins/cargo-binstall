use std::{borrow::Cow, ops::Deref, str::FromStr};

use binstalk_types::cargo_toml_binstall::TargetTriple as TargetTripleInner;

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
            "target-arch" => Some(self.arch.into_str()),
            "target-libc" => Some(self.env.into_str()),
            "target-vendor" => Some(Cow::Borrowed(self.vendor.as_str())),

            _ => None,
        }
    }
}
