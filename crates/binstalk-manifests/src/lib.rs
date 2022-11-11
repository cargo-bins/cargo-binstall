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
pub mod cargo_crates_v1;
pub mod cargo_toml_binstall;
pub mod crate_info;
