use serde::{Serialize, Deserialize};
use strum_macros::{Display, EnumString, EnumVariantNames};
use tinytemplate::TinyTemplate;


pub mod helpers;
pub use helpers::*;

pub mod drivers;
pub use drivers::*;


/// Compiled target triple, used as default for binary fetching
pub const TARGET: &'static str = env!("TARGET");

/// Default package path template (may be overridden in package Cargo.toml)
pub const DEFAULT_PKG_URL: &'static str = "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ format }";

/// Default binary name template (may be overridden in package Cargo.toml)
pub const DEFAULT_BIN_PATH: &'static str = "{ name }-{ target }-v{ version }/{ bin }{ format }";


/// Binary format enumeration
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[derive(Display, EnumString, EnumVariantNames)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PkgFmt {
    /// Download format is TAR (uncompressed)
    Tar,
    /// Download format is TGZ (TAR + GZip)
    Tgz,
    /// Download format is raw / binary
    Bin,
}

impl Default for PkgFmt {
    fn default() -> Self {
        Self::Tgz
    }
}

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
}

impl Default for PkgMeta {
    fn default() -> Self {
        Self {
            pkg_url: DEFAULT_PKG_URL.to_string(),
            pkg_fmt: PkgFmt::default(),
            bin_dir: DEFAULT_BIN_PATH.to_string(),
            pub_key: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BinMeta {
    /// Binary name
    pub name: String,
    /// Binary template path (within package)
    pub path: String,
}

/// Template for constructing download paths
#[derive(Clone, Debug, Serialize)]
pub struct Context {
    pub name: String,
    pub repo: Option<String>,
    pub target: String,
    pub version: String,
    pub format: String,
    pub bin: Option<String>,
}

impl Context {
    /// Render the context into the provided template
    pub fn render(&self, template: &str) -> Result<String, anyhow::Error> {
        // Create template instance
        let mut tt = TinyTemplate::new();

        // Add template to instance
        tt.add_template("path", &template)?;

        // Render output
        let rendered = tt.render("path", self)?;

        Ok(rendered)
    }
}

#[cfg(test)]
mod test {
    use crate::{load_manifest_path};

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
        let meta = package.metadata.map(|m| m.binstall ).flatten().unwrap();

        assert_eq!(&package.name, "cargo-binstall");

        assert_eq!(
            &meta.pkg_url,
            "{ repo }/releases/download/v{ version }/{ name }-{ target }.tgz"
        );

        assert_eq!(
            manifest.bin.as_slice(),
            &[
                Product{ 
                    name: Some("cargo-binstall".to_string()),
                    path: Some("src/main.rs".to_string()),
                    edition: Some(cargo_toml::Edition::E2018),
                    ..Default::default()
                },
            ],
        );
    }
}
