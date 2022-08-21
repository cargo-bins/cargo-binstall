mod detect;
pub use detect::detect_targets;

mod desired_targets;
pub use desired_targets::{get_desired_targets, DesiredTargets};

/// Compiled target triple, used as default for binary fetching
pub const TARGET: &str = env!("TARGET");
