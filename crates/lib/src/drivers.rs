mod version;
use version::find_version;

mod crates_io;
pub use crates_io::fetch_crate_cratesio;
