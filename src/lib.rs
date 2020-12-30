use structopt::StructOpt;
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
pub const DEFAULT_PKG_PATH: &'static str = "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ format }";

/// Default binary name template (may be overridden in package Cargo.toml)
pub const DEFAULT_BIN_NAME: &'static str = "{ name }-{ target }-v{ version }";


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

/// Metadata for binary installation use.
/// 
/// Exposed via `[package.metadata]` in `Cargo.toml`
#[derive(Clone, Debug, StructOpt, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Meta {
    /// Path template override for package downloads
    pub pkg_url: Option<String>,

    /// Package name override for package downloads
    pub pkg_name: Option<String>,

    /// Format override for package downloads
    #[serde(default)]
    pub pkg_fmt: Option<PkgFmt>,

    #[serde(default)]
    /// Filters for binary files allowed in the package
    pub pkg_bins: Vec<String>,

    /// Public key for package verification (base64 encoded)
    pub pub_key: Option<String>,
}

/// Template for constructing download paths
#[derive(Clone, Debug, Serialize)]
pub struct Context {
    pub name: String,
    pub repo: Option<String>,
    pub target: String,
    pub version: String,
    pub format: String,
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

