//! The format of the `[package.metadata.binstall]` manifest.
//!
//! This manifest defines how a particular binary crate may be installed by Binstall.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[doc(inline)]
pub use package_formats::*;

mod package_formats;

/// Default package path template (may be overridden in package Cargo.toml)
pub const DEFAULT_PKG_URL: &str =
    "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ archive-format }";

/// Default binary name template (may be overridden in package Cargo.toml)
pub const DEFAULT_BIN_DIR: &str = "{ name }-{ target }-v{ version }/{ bin }{ binary-ext }";

/// `binstall` metadata container
///
/// Required to nest metadata under `package.metadata.binstall`
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Meta {
    pub binstall: Option<PkgMeta>,
}

/// Metadata for binary installation use.
///
/// Exposed via `[package.metadata]` in `Cargo.toml`
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PkgMeta {
    /// URL template for package downloads
    pub pkg_url: String,

    /// Format for package downloads
    pub pkg_fmt: Option<PkgFmt>,

    /// Path template for binary files in packages
    pub bin_dir: String,

    /// Public key for package verification (base64 encoded)
    pub pub_key: Option<String>,

    /// Target specific overrides
    pub overrides: HashMap<String, PkgOverride>,
}

impl Default for PkgMeta {
    fn default() -> Self {
        Self {
            pkg_url: DEFAULT_PKG_URL.to_string(),
            pkg_fmt: None,
            bin_dir: DEFAULT_BIN_DIR.to_string(),
            pub_key: None,
            overrides: HashMap::new(),
        }
    }
}

impl PkgMeta {
    pub fn clone_without_overrides(&self) -> Self {
        Self {
            pkg_url: self.pkg_url.clone(),
            pkg_fmt: self.pkg_fmt,
            bin_dir: self.bin_dir.clone(),
            pub_key: self.pub_key.clone(),
            overrides: HashMap::new(),
        }
    }

    /// Merge configuration overrides into object
    pub fn merge(&mut self, pkg_override: &PkgOverride) {
        if let Some(o) = &pkg_override.pkg_url {
            self.pkg_url = o.clone();
        }
        if let Some(o) = &pkg_override.pkg_fmt {
            self.pkg_fmt = Some(*o);
        }
        if let Some(o) = &pkg_override.bin_dir {
            self.bin_dir = o.clone();
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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BinMeta {
    /// Binary name
    pub name: String,
    /// Binary template path (within package)
    pub path: String,
}
