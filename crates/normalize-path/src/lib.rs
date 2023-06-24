//! Normalizes paths similarly to canonicalize, but without performing I/O.
//!
//! This is like Python's `os.path.normpath`.
//!
//! Initially adapted from [Cargo's implementation][cargo-paths].
//!
//! [cargo-paths]: https://github.com/rust-lang/cargo/blob/fede83ccf973457de319ba6fa0e36ead454d2e20/src/cargo/util/paths.rs#L61
//!
//! # Example
//!
//! ```
//! use normalize_path::NormalizePath;
//! use std::path::Path;
//!
//! assert_eq!(
//!     Path::new("/A/foo/../B/./").normalize(),
//!     Path::new("/A/B")
//! );
//! ```

use std::path::{Component, Path, PathBuf};

/// Extension trait to add `normalize_path` to std's [`Path`].
pub trait NormalizePath {
    /// Normalize a path without performing I/O.
    ///
    /// All redundant separator and up-level references are collapsed.
    ///
    /// However, this does not resolve links.
    fn normalize(&self) -> PathBuf;

    /// Same as [`NormalizePath::normalize`] except that if
    /// `Component::Prefix`/`Component::RootDir` is encountered,
    /// or if the path points outside of current dir, returns `None`.
    fn try_normalize(&self) -> Option<PathBuf>;

    /// Return `true` if the path is normalized.
    ///
    /// # Quirk
    ///
    /// If the path does not start with `./` but contains `./` in the middle,
    /// then this function might returns `true`.
    fn is_normalized(&self) -> bool;
}

impl NormalizePath for Path {
    fn normalize(&self) -> PathBuf {
        let mut components = self.components().peekable();
        let mut ret = if let Some(c @ Component::Prefix(..)) = components.peek() {
            let buf = PathBuf::from(c.as_os_str());
            components.next();
            buf
        } else {
            PathBuf::new()
        };

        for component in components {
            match component {
                Component::Prefix(..) => unreachable!(),
                Component::RootDir => {
                    ret.push(component.as_os_str());
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    ret.pop();
                }
                Component::Normal(c) => {
                    ret.push(c);
                }
            }
        }

        ret
    }

    fn try_normalize(&self) -> Option<PathBuf> {
        let mut ret = PathBuf::new();

        for component in self.components() {
            match component {
                Component::Prefix(..) | Component::RootDir => return None,
                Component::CurDir => {}
                Component::ParentDir => {
                    if !ret.pop() {
                        return None;
                    }
                }
                Component::Normal(c) => {
                    ret.push(c);
                }
            }
        }

        Some(ret)
    }

    fn is_normalized(&self) -> bool {
        for component in self.components() {
            match component {
                Component::CurDir | Component::ParentDir => {
                    return false;
                }
                _ => continue,
            }
        }

        true
    }
}
