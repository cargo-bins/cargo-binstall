#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod args;
mod bin_util;
mod entry;
mod git_credentials;
mod install_path;
mod manifests;
mod signal;
mod ui;

mod do_main;
pub use do_main::main_impl;
