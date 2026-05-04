//! The format of the `[package.metadata.binstall]` manifest.
//!
//! This manifest defines how a particular binary crate may be installed by
//! Binstall.
//!
//! A useful way to read this schema is:
//!
//! - metadata describes where files live in the published artifact
//! - binstall decides where those files should be installed locally
//!
//! That split is especially important for extra files such as man pages and
//! shell completions. The manifest can say "the zsh completion is at
//! `completions/zsh/_{ bin }` in the archive", but it does not attempt to
//! encode arbitrary host destination policy.

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

    /// Source paths for extra installed files.
    ///
    /// These are archive-relative templates, not destination paths.
    /// Binstall intentionally fixes the install destinations under Cargo's
    /// `share/` tree so crates can describe where files live in the archive
    /// without also needing to encode host-specific filesystem policy.
    #[serde(flatten)]
    pub extra_files: PkgExtraFiles,

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
        self.extra_files.merge(&pkg_override.extra_files);
    }

    /// Merge configuration overrides into object.
    ///
    /// The iterator order matters: the first matching non-`None` value wins.
    /// Callers are expected to pass exact-target matches before `cfg(...)`
    /// matches so this preserves the precedence documented in `SUPPORT.md`.
    ///
    ///  * `pkg_overrides` - ordered in preference
    pub fn merge_overrides<'a, It>(&self, pkg_overrides: It) -> Self
    where
        It: IntoIterator<Item = &'a PkgOverride> + Clone,
    {
        let extra_file_overrides = pkg_overrides
            .clone()
            .into_iter()
            .map(|pkg_override| &pkg_override.extra_files)
            .collect::<Vec<_>>();

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

            extra_files: self.extra_files.merge_overrides(extra_file_overrides),

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

    /// Source path overrides for extra installed files.
    ///
    /// These follow the same precedence rules as `pkg-url` / `bin-dir` and
    /// are kept grouped so target overrides can replace one extra-file type
    /// without restating the others.
    #[serde(flatten)]
    pub extra_files: PkgExtraFiles,

    /// Strategies to disable
    pub disabled_strategies: Option<Box<[Strategy]>>,

    /// Package signing configuration
    pub signing: Option<PkgSigning>,

    #[serde(skip)]
    pub ignore_disabled_strategies: bool,
}

/// Source path templates for extra installed files.
///
/// Each field points at the file location inside the fetched archive.
/// Destination paths are derived from the file kind and binary name so that:
///
/// - crates describe packaging layout, not host install policy
/// - installs remain consistent across repositories
/// - tracked cleanup can key off a stable relative destination
///
/// These fields are intentionally about source discovery only. Reviewers may
/// reasonably ask why there is no companion "destination override" here; the
/// answer is that binstall currently treats install locations as part of its
/// own policy so packages cannot redirect files outside the conventional Cargo
/// layout.
///
/// For crates with multiple binaries, these templates are rendered once per
/// selected binary using that binary's `{ bin }` value. That means a constant
/// template such as `man = "docs/tool.1"` is treated as "every installed
/// binary uses the same source file" and may therefore collide at destination
/// time if more than one binary resolves to the same installed path.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PkgExtraFiles {
    /// Man page source path template within package archive
    pub man: Option<String>,

    /// Bash completion source path template within package archive
    pub bash_completion: Option<String>,

    /// Fish completion source path template within package archive
    pub fish_completion: Option<String>,

    /// Zsh completion source path template within package archive
    pub zsh_completion: Option<String>,
}

impl PkgExtraFiles {
    /// Merge a single override into this set.
    ///
    /// We merge field-by-field so a target override can replace only the zsh
    /// completion path, for example, while inheriting the default man page
    /// and other completion locations from the base metadata.
    pub fn merge(&mut self, other: &Self) {
        if let Some(man) = &other.man {
            self.man = Some(man.clone());
        }
        if let Some(bash_completion) = &other.bash_completion {
            self.bash_completion = Some(bash_completion.clone());
        }
        if let Some(fish_completion) = &other.fish_completion {
            self.fish_completion = Some(fish_completion.clone());
        }
        if let Some(zsh_completion) = &other.zsh_completion {
            self.zsh_completion = Some(zsh_completion.clone());
        }
    }

    pub fn merge_overrides<'a, It>(&self, overrides: It) -> Self
    where
        It: IntoIterator<Item = &'a PkgExtraFiles> + Clone,
    {
        // Clone is required because precedence is independent per field:
        // the first override that sets `man` may differ from the first that
        // sets `bash-completion`.
        Self {
            man: overrides
                .clone()
                .into_iter()
                .find_map(|extra_files| extra_files.man.clone())
                .or_else(|| self.man.clone()),
            bash_completion: overrides
                .clone()
                .into_iter()
                .find_map(|extra_files| extra_files.bash_completion.clone())
                .or_else(|| self.bash_completion.clone()),
            fish_completion: overrides
                .clone()
                .into_iter()
                .find_map(|extra_files| extra_files.fish_completion.clone())
                .or_else(|| self.fish_completion.clone()),
            zsh_completion: overrides
                .into_iter()
                .find_map(|extra_files| extra_files.zsh_completion.clone())
                .or_else(|| self.zsh_completion.clone()),
        }
    }
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

    #[test]
    fn test_extra_files_parse_and_merge() {
        let meta: PkgMeta = serde_json::from_value(json!({
            "man": "docs/man/{ bin }.1",
            "bash-completion": "completions/bash/{ bin }",
            "overrides": {
                "x86_64-unknown-linux-gnu": {
                    "fish-completion": "completions/fish/{ bin }.fish"
                }
            }
        }))
        .unwrap();

        assert_eq!(meta.extra_files.man.as_deref(), Some("docs/man/{ bin }.1"));
        assert_eq!(
            meta.extra_files.bash_completion.as_deref(),
            Some("completions/bash/{ bin }")
        );

        let cfgs = vec![
            Cfg::from_str(r#"target_os="linux""#).unwrap(),
            Cfg::from_str(r#"target_arch="x86_64""#).unwrap(),
            Cfg::from_str("unix").unwrap(),
        ];

        let merged = meta.merge_overrides(
            meta.overrides
                .get_matching("x86_64-unknown-linux-gnu", &cfgs),
        );

        assert_eq!(
            merged.extra_files.fish_completion.as_deref(),
            Some("completions/fish/{ bin }.fish")
        );
        assert_eq!(
            merged.extra_files.man.as_deref(),
            Some("docs/man/{ bin }.1")
        );
    }
}
