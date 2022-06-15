//! Shamelessly adapted from:
//! https://github.com/rust-lang/cargo/blob/fede83ccf973457de319ba6fa0e36ead454d2e20/src/cargo/util/paths.rs#L61

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

pub trait PathExt {
    /// Similiar to `os.path.normpath`: It does not perform
    /// any fs operation.
    fn normalize_path(&self) -> Cow<'_, Path>;
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

impl PathExt for Path {
    fn normalize_path(&self) -> Cow<'_, Path> {
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
