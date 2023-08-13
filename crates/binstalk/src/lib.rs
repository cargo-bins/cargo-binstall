#![cfg_attr(docsrs, feature(doc_auto_cfg))]

mod bins;
pub mod errors;
pub mod fetchers;
pub mod helpers;
pub mod ops;

use atomic_file_install as fs;
pub use binstalk_registry as registry;
pub use binstalk_types as manifests;
pub use detect_targets::{get_desired_targets, DesiredTargets, TARGET};
pub use home;
