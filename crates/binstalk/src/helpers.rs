pub mod jobserver_client;
pub mod logging;
pub mod remote;
pub(crate) mod target_triple;
pub mod tasks;

pub(crate) use binstalk_downloader::download;
pub use binstalk_downloader::gh_api_client;

#[cfg(feature = "git")]
pub(crate) use binstalk_downloader::git;
pub(crate) use cargo_toml_workspace::{self, cargo_toml};

pub(crate) fn is_universal_macos(target: &str) -> bool {
    ["universal-apple-darwin", "universal2-apple-darwin"].contains(&target)
}
