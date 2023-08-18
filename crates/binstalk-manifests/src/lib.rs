//! Manifest formats and utilities.
//!
//! There are three types of manifests Binstall may deal with:
//! - manifests that define how to fetch and install a package
//!   ([Cargo.toml's `[metadata.binstall]`][cargo_toml_binstall]);
//! - manifests that record which packages _are_ installed
//!   ([Cargo's `.crates.toml`][cargo_crates_v1] and
//!   [Binstall's `.crates-v1.json`][binstall_crates_v1]);
//! - manifests that specify which packages _to_ install (currently none).

mod helpers;

pub mod binstall_crates_v1;
pub mod cargo_config;
pub mod cargo_crates_v1;
/// Contains both [`binstall_crates_v1`] and [`cargo_crates_v1`].
pub mod crates_manifests;

pub use binstalk_types::{cargo_toml_binstall, crate_info};
pub use compact_str::CompactString;
pub use semver::Version;
pub use url::Url;
