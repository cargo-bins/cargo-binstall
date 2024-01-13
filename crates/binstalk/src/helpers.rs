pub mod jobserver_client;
pub mod remote;
pub(crate) mod target_triple;
pub mod tasks;

pub(crate) use binstalk_downloader::download;
pub use binstalk_downloader::{cacher, gh_api_client};

pub(crate) use cargo_toml_workspace::{self, cargo_toml};
#[cfg(feature = "git")]
pub(crate) use simple_git as git;

pub(crate) fn is_universal_macos(target: &str) -> bool {
    ["universal-apple-darwin", "universal2-apple-darwin"].contains(&target)
}
