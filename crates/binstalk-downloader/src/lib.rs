#![cfg_attr(docsrs, feature(doc_auto_cfg))]

pub use bytes;

pub mod download;

/// Github API client.
/// Currently only support github.com and does not support other enterprise
/// github.
#[cfg(feature = "gh-api-client")]
pub mod gh_api_client;

pub mod remote;

mod utils;
