//! Detect the target at the runtime.
//!
//! Example use cases:
//!  - The binary is built with musl libc to run on anywhere, but
//!    the runtime supports glibc.
//!  - The binary is built for x86_64-apple-darwin, but run on
//!    aarch64-apple-darwin.

mod detect;
pub use detect::detect_targets;

mod desired_targets;
pub use desired_targets::{get_desired_targets, DesiredTargets};

/// Compiled target triple, used as default for binary fetching
pub const TARGET: &str = env!("TARGET");
