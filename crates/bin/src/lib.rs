#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/assets/cargo-binstall-logo.svg"
)]

mod args;
mod bin_util;
mod entry;
mod gh_token;
mod git_credentials;
mod initialise;
mod logging;
mod main_impl;
mod registry_auth;
mod settings;
mod signal;
mod ui;

pub use main_impl::do_main;
