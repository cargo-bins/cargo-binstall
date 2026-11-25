#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc(
    html_logo_url = "https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/assets/cargo-binstall-logo.svg"
)]

pub mod errors;
pub mod helpers;
pub mod ops;

use binstalk_bins as bins;
pub use binstalk_fetchers as fetchers;
pub use binstalk_registry as registry;
pub use binstalk_types as manifests;
pub use detect_targets::{DesiredTargets, TARGET, get_desired_targets};

pub use fetchers::QUICKINSTALL_STATS_URL;
