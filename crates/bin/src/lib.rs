#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod args;
mod bin_util;
mod entry;
mod gh_token;
mod git_credentials;
mod initialise;
mod logging;
mod main_impl;
mod settings;
mod signal;
mod ui;

pub use main_impl::do_main;
