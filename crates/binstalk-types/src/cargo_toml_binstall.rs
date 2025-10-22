//! The format of the `[package.metadata.binstall]` manifest.
//!
//! This manifest defines how a particular binary crate may be installed by Binstall.

use std::{borrow::Cow, collections::BTreeMap};

use serde::{Deserialize, Serialize};
use strum_macros::{EnumCount, VariantArray};

mod package_formats;
#[doc(inline)]
pub use package_formats::*;

/// `binstall` metadata container
///
/// Required to nest metadata under `package.metadata.binstall`
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Meta {
    pub binstall: Option<PkgMeta>,
}

/// Strategies to use for binary discovery
#[derive(
    Debug,
    Copy,
    Clone,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    EnumCount,
    VariantArray,
    Deserialize,
    Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    /// Attempt to download official pre-built artifacts using
    /// information provided in `Cargo.toml`.
    CrateMetaData,
    /// Query third-party QuickInstall for the crates.
    QuickInstall,
    /// Build the crates from source using `cargo-build`.
    Compile,
}

impl Strategy {
    pub const fn to_str(self) -> &'static str {
        match self {
            Strategy::CrateMetaData => "crate-meta-data",
            Strategy::QuickInstall => "quick-install",
            Strategy::Compile => "compile",
        }
    }
}

/// Metadata for binary installation use.
///
/// Exposed via `[package.metadata]` in `Cargo.toml`
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PkgMeta {
    /// URL template for package downloads
    pub pkg_url: Option<String>,

    /// Format for package downloads
    pub pkg_fmt: Option<PkgFmt>,

    /// Path template for binary files in packages
    pub bin_dir: Option<String>,

    /// Package signing configuration
    pub signing: Option<PkgSigning>,

    /// Strategies to disable
    pub disabled_strategies: Option<Box<[Strategy]>>,

    /// Target specific overrides
    pub overrides: BTreeMap<String, PkgOverride>,
}

impl PkgMeta {
    /// Merge configuration overrides into object
    pub fn merge(&mut self, pkg_override: &PkgOverride) {
        if let Some(o) = &pkg_override.pkg_url {
            self.pkg_url = Some(o.clone());
        }
        if let Some(o) = &pkg_override.pkg_fmt {
            self.pkg_fmt = Some(*o);
        }
        if let Some(o) = &pkg_override.bin_dir {
            self.bin_dir = Some(o.clone());
        }
    }

    /// Merge configuration overrides into object
    ///
    ///  * `pkg_overrides` - ordered in preference
    pub fn merge_overrides<'a, It>(&self, pkg_overrides: It) -> Self
    where
        It: IntoIterator<Item = &'a PkgOverride> + Clone,
    {
        let ignore_disabled_strategies = pkg_overrides
            .clone()
            .into_iter()
            .any(|pkg_override| pkg_override.ignore_disabled_strategies);

        Self {
            pkg_url: pkg_overrides
                .clone()
                .into_iter()
                .find_map(|pkg_override| pkg_override.pkg_url.clone())
                .or_else(|| self.pkg_url.clone()),

            pkg_fmt: pkg_overrides
                .clone()
                .into_iter()
                .find_map(|pkg_override| pkg_override.pkg_fmt)
                .or(self.pkg_fmt),

            bin_dir: pkg_overrides
                .clone()
                .into_iter()
                .find_map(|pkg_override| pkg_override.bin_dir.clone())
                .or_else(|| self.bin_dir.clone()),

            signing: pkg_overrides
                .clone()
                .into_iter()
                .find_map(|pkg_override| pkg_override.signing.clone())
                .or_else(|| self.signing.clone()),

            disabled_strategies: if ignore_disabled_strategies {
                None
            } else {
                let mut disabled_strategies = pkg_overrides
                    .into_iter()
                    .filter_map(|pkg_override| pkg_override.disabled_strategies.as_deref())
                    .flatten()
                    .chain(self.disabled_strategies.as_deref().into_iter().flatten())
                    .copied()
                    .collect::<Vec<Strategy>>();

                disabled_strategies.sort_unstable();
                disabled_strategies.dedup();

                Some(disabled_strategies.into_boxed_slice())
            },

            overrides: Default::default(),
        }
    }
}

/// Target specific overrides for binary installation
///
/// Exposed via `[package.metadata.TARGET]` in `Cargo.toml`
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PkgOverride {
    /// URL template override for package downloads
    pub pkg_url: Option<String>,

    /// Format override for package downloads
    pub pkg_fmt: Option<PkgFmt>,

    /// Path template override for binary files in packages
    pub bin_dir: Option<String>,

    /// Strategies to disable
    pub disabled_strategies: Option<Box<[Strategy]>>,

    /// Package signing configuration
    pub signing: Option<PkgSigning>,

    #[serde(skip)]
    pub ignore_disabled_strategies: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BinMeta {
    /// Binary name
    pub name: String,

    /// Binary template (path within package)
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PkgSigning {
    /// Signing algorithm supported by Binstall.
    pub algorithm: SigningAlgorithm,

    /// Signing public key
    pub pubkey: Cow<'static, str>,

    /// Signature file override template (url to download)
    #[serde(default)]
    pub file: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum SigningAlgorithm {
    /// [minisign](https://jedisct1.github.io/minisign/)
    Minisign,
}

#[cfg(test)]
mod tests {
    use strum::VariantArray;

    use super::*;

    #[test]
    fn test_strategy_ser() {
        Strategy::VARIANTS.iter().for_each(|strategy| {
            assert_eq!(
                serde_json::to_string(&strategy).unwrap(),
                format!(r#""{}""#, strategy.to_str())
            )
        });
    }
}
