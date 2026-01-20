//! The format of the `[package.metadata.binstall]` manifest.
//!
//! This manifest defines how a particular binary crate may be installed by Binstall.

use std::borrow::Cow;

use cargo_platform::{Cfg, Platform};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use strum_macros::{EnumCount, VariantArray};

mod package_formats;
mod target_triple;

#[doc(inline)]
pub use package_formats::*;
#[doc(inline)]
pub use target_triple::*;

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
    pub overrides: PkgOverrides,
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

/// A target-specific override for binary installation.
///
/// Exposed via `[package.metadata.overrides.TARGET]` in `Cargo.toml`.
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

/// An ordered map of target-specific overrides.
///
/// Exposed via `[package.metadata.overrides]` in `Cargo.toml`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PkgOverrides(IndexMap<Platform, PkgOverride>);

impl PkgOverrides {
    /// Returns all matching overrides for a target, in order of precedence:
    /// exact name matches first, followed by `cfg(...)` matches
    /// in declaration order.
    pub fn get_matching<'a>(
        &'a self,
        target: &'a str,
        cfgs: &'a [Cfg],
    ) -> impl Iterator<Item = &'a PkgOverride> + Clone {
        let name = self.0.get(&Platform::Name(target.to_owned()));
        let cfgs = self.0.iter().filter_map(|(p, o)| match p {
            Platform::Cfg(p) if p.matches(cfgs) => Some(o),
            _ => None,
        });
        name.into_iter().chain(cfgs)
    }
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
    use std::str::FromStr;

    use serde_json::json;
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

    #[test]
    fn test_pkg_overrides_parse_target_name() {
        let json = json!({
            "x86_64-unknown-linux-gnu": {
                "pkg-fmt": "tgz",
            },
        });
        let overrides: PkgOverrides = serde_json::from_value(json).unwrap();
        assert!(matches!(overrides, PkgOverrides(map) if !map.is_empty()));
    }

    #[test]
    fn test_pkg_overrides_parse_cfg_expression() {
        let json = json!({
            r#"cfg(target_os = "linux")"#: {
                "pkg-fmt": "tgz",
            },
        });
        let overrides: PkgOverrides = serde_json::from_value(json).unwrap();
        assert!(matches!(overrides, PkgOverrides(map) if !map.is_empty()));
    }

    #[test]
    fn test_pkg_overrides_parse_mixed() {
        let json = json!({
            "x86_64-unknown-linux-gnu": {
                "pkg-fmt": "tar",
            },
            r#"cfg(target_os = "linux")"#: {
                "pkg-fmt": "tgz",
            },
            "cfg(unix)": {
                "bin-dir": "bin",
            },
        });
        let overrides: PkgOverrides = serde_json::from_value(json).unwrap();
        assert!(matches!(overrides, PkgOverrides(map) if !map.is_empty()));
    }

    #[test]
    fn test_pkg_overrides_exact_match_precedence() {
        let json = json!({
            r#"cfg(target_os = "linux")"#: {
                "pkg-fmt": "tgz",
            },
            "x86_64-unknown-linux-gnu": {
                "pkg-fmt": "tar",
            },
        });
        let overrides: PkgOverrides = serde_json::from_value(json).unwrap();

        // `x86_64-unknown-linux-gnu` should match all these.
        let cfgs = vec![
            Cfg::from_str(r#"target_os="linux""#).unwrap(),
            Cfg::from_str(r#"target_arch="x86_64""#).unwrap(),
            Cfg::from_str("unix").unwrap(),
        ];

        let matches: Vec<_> = overrides
            .get_matching("x86_64-unknown-linux-gnu", &cfgs)
            .collect();

        // The exact match should come first, followed by the `cfg` match.
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].pkg_fmt, Some(PkgFmt::Tar));
        assert_eq!(matches[1].pkg_fmt, Some(PkgFmt::Tgz));
    }

    #[test]
    fn test_pkg_overrides_cfg_match_order() {
        // `serde_json::Map` is backed by a `BTreeMap` by default,
        // so we parse directly from a JSON string instead of using
        // the `json!(...)` macro.
        let json = r#"{
            "cfg(unix)": {
                "pkg-fmt": "tgz"
            },
            "cfg(target_os = \"linux\")": {
                "pkg-fmt": "tar"
            }
        }"#;
        let overrides: PkgOverrides = serde_json::from_str(json).unwrap();

        let cfgs = vec![
            Cfg::from_str(r#"target_os="linux""#).unwrap(),
            Cfg::from_str("unix").unwrap(),
        ];

        let matches: Vec<_> = overrides
            .get_matching("x86_64-unknown-linux-gnu", &cfgs)
            .collect();

        // `cfg` matches should be returned in declaration order.
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].pkg_fmt, Some(PkgFmt::Tgz)); // Unix.
        assert_eq!(matches[1].pkg_fmt, Some(PkgFmt::Tar)); // Linux.
    }

    #[test]
    fn test_pkg_overrides_no_match() {
        let json = json!({
            r#"cfg(target_os = "windows")"#: {
                "pkg-fmt": "zip",
            },
        });
        let overrides: PkgOverrides = serde_json::from_value(json).unwrap();

        let cfgs = vec![
            Cfg::from_str(r#"target_os="linux""#).unwrap(),
            Cfg::from_str("unix").unwrap(),
        ];

        let matches: Vec<_> = overrides
            .get_matching("x86_64-unknown-linux-gnu", &cfgs)
            .collect();

        assert!(matches.is_empty());
    }
}
