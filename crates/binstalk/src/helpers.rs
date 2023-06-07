pub mod futures_resolver;
pub mod jobserver_client;
pub mod remote;
pub mod signal;
pub mod target_triple;
pub mod tasks;

pub use binstalk_downloader::{download, gh_api_client};

pub fn is_universal_macos(target: &str) -> bool {
    ["universal-apple-darwin", "universal2-apple-darwin"].contains(&target)
}
