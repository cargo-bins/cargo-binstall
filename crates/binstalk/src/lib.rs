#![cfg_attr(docsrs, feature(doc_auto_cfg))]

pub mod bins;
pub mod drivers;
pub mod errors;
pub mod fetchers;
pub mod fs;
pub mod helpers;
pub mod ops;

pub use binstalk_types as manifests;
pub use detect_targets::{get_desired_targets, DesiredTargets, TARGET};
pub use home;
