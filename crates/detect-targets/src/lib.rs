//! Detect the target at the runtime.
//!
//! It runs `$CARGO -vV` if environment variable `CARGO` is present
//! for cargo subcommands, otherwise it would try running `rustc -vV`.
//!
//! If both `rustc` isn't present on the system, it will fallback
//! to using syscalls plus `ldd` on Linux to detect targets.
//!
//! Example use cases:
//!  - The binary is built with musl libc to run on anywhere, but
//!    the runtime supports glibc.
//!  - The binary is built for x86_64-apple-darwin, but run on
//!    aarch64-apple-darwin.
//!
//! This crate provides two API:
//!  - [`detect_targets`] provides the API to get the target
//!    at runtime, but the code is run on the current thread.
//!  - [`get_desired_targets`] provides the API to either
//!    use override provided by the users, or run [`detect_targets`]
//!    in the background using [`tokio::spawn`].
//!
//! # Example
//!
//! `detect_targets`:
//!
//! ```rust
//! use detect_targets::detect_targets;
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//!
//! let targets = detect_targets().await;
//! eprintln!("Your platform supports targets: {targets:#?}");
//! # }
//! ```
//!
//! `get_desired_targets` with user override:
//!
//! ```rust
//! use detect_targets::get_desired_targets;
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//!
//! assert_eq!(
//!     get_desired_targets(Some(vec![
//!         "x86_64-apple-darwin".to_string(),
//!         "aarch64-apple-darwin".to_string(),
//!     ])).get().await,
//!     &["x86_64-apple-darwin", "aarch64-apple-darwin"],
//! );
//! # }
//! ```
//!
//! `get_desired_targets` without user override:
//!
//! ```rust
//! use detect_targets::get_desired_targets;
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//!
//! eprintln!(
//!     "Your platform supports targets: {:#?}",
//!     get_desired_targets(None).get().await
//! );
//! # }
//! ```

mod detect;
pub use detect::detect_targets;

mod desired_targets;
pub use desired_targets::{get_desired_targets, DesiredTargets};

/// Compiled target triple, used as default for binary fetching
pub const TARGET: &str = env!("TARGET");
