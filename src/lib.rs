use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub mod drivers;
pub use drivers::*;

pub mod errors;
pub use errors::*;

pub mod helpers;
pub use helpers::*;

pub mod bins;
pub mod fetchers;

mod target;
pub use target::*;

mod formats;
pub use formats::*;

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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PkgMeta {
    /// URL template for package downloads
    pub pkg_url: String,

    /// Format for package downloads
    pub pkg_fmt: PkgFmt,

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
            pkg_fmt: PkgFmt::default(),
            bin_dir: DEFAULT_BIN_DIR.to_string(),
            pub_key: None,
            overrides: HashMap::new(),
        }
    }
}

impl PkgMeta {
    /// Merge configuration overrides into object
    pub fn merge(&mut self, pkg_override: &PkgOverride) {
        if let Some(o) = &pkg_override.pkg_url {
            self.pkg_url = o.clone();
        }
        if let Some(o) = &pkg_override.pkg_fmt {
            self.pkg_fmt = *o;
        }
        if let Some(o) = &pkg_override.bin_dir {
            self.bin_dir = o.clone();
        }
    }
}

/// Target specific overrides for binary installation
///
/// Exposed via `[package.metadata.TARGET]` in `Cargo.toml`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
pub struct PkgOverride {
    /// URL template override for package downloads
    pub pkg_url: Option<String>,

    /// Format override for package downloads
    pub pkg_fmt: Option<PkgFmt>,

    /// Path template override for binary files in packages
    pub bin_dir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BinMeta {
    /// Binary name
    pub name: String,
    /// Binary template path (within package)
    pub path: String,
}

#[cfg(test)]
mod test {
    use crate::load_manifest_path;

    use cargo_toml::Product;

    fn init() {
        let _ = env_logger::builder().is_test(true).try_init();
    }

    #[test]
    fn parse_meta() {
        init();

        let mut manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        manifest_dir.push_str("/Cargo.toml");

        let manifest = load_manifest_path(&manifest_dir).expect("Error parsing metadata");
        let package = manifest.package.unwrap();
        let meta = package.metadata.and_then(|m| m.binstall).unwrap();

        assert_eq!(&package.name, "cargo-binstall");

        assert_eq!(
            &meta.pkg_url,
            "{ repo }/releases/download/v{ version }/{ name }-{ target }.{ format }"
        );

        assert_eq!(
            manifest.bin.as_slice(),
            &[Product {
                name: Some("cargo-binstall".to_string()),
                path: Some("src/main.rs".to_string()),
                edition: Some(cargo_toml::Edition::E2021),
                ..Default::default()
            },],
        );
    }
}
