pub(crate) mod cargo_toml_workspace;
pub(crate) mod futures_resolver;
#[cfg(feature = "git")]
pub(crate) mod git;
pub mod jobserver_client;
pub mod remote;
pub(crate) mod target_triple;
pub mod tasks;

pub use binstalk_downloader::gh_api_client;
pub(crate) use binstalk_downloader::{bytes, download};

pub(crate) fn is_universal_macos(target: &str) -> bool {
    ["universal-apple-darwin", "universal2-apple-darwin"].contains(&target)
}
