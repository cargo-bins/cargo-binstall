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
//!
//! assert_eq!(
//!     Path::new("/A/foo/../B/./").normalize(),
//!     Path::new("/A/B")
//! );
//! ```

use std::{
    borrow::Cow,
    path::{Component, Path, PathBuf},
};

/// Extension trait to add `normalize_path` to std's [`Path`].
pub trait NormalizePath {
    /// Normalize a path without performing I/O.
    ///
    /// All redundant separator and up-level references are collapsed.
    ///
    /// However, this does not resolve links.
    fn normalize(&self) -> Cow<'_, Path>;
}

fn is_normalized(path: &Path) -> bool {
    for component in path.components() {
        match component {
            Component::CurDir | Component::ParentDir => {
                return false;
            }
            _ => continue,
        }
    }

    true
}

impl NormalizePath for Path {
    fn normalize(&self) -> Cow<'_, Path> {
        if is_normalized(self) {
            return Cow::Borrowed(self);
        }

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
        Cow::Owned(ret)
    }
}
