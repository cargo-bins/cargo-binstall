#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod args;
mod bin_util;
mod entry;
mod install_path;
mod main_impl;
mod signal;
mod ui;

pub use main_impl::do_main;
