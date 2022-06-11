use std::collections::{hash_map::HashMap, hash_set::HashSet};
use std::io;
use std::path::Path;

use cargo_toml::AbstractFilesystem;

use crate::helpers::PathExt;

#[derive(Debug)]
pub(super) struct Vfs(HashMap<Box<Path>, HashSet<Box<str>>>);

impl Vfs {
    pub(super) fn new() -> Self {
        Self(HashMap::with_capacity(16))
    }

    /// * `path` - must be canonical, must not be empty and must
    ///   start with a prefix.
    pub(super) fn add_path(&mut self, mut path: &Path) {
        while let Some(parent) = path.parent() {
            if let Some(path_str) = path.to_str() {
                self.0
                    .entry(parent.into())
                    .or_insert_with(|| HashSet::with_capacity(4))
                    .insert(path_str.into());
            }

            path = parent;
        }
    }
}

impl AbstractFilesystem for Vfs {
    fn file_names_in(&self, rel_path: &str) -> io::Result<HashSet<Box<str>>> {
        let rel_path = Path::new(rel_path).normalize_path();

        Ok(self.0.get(&*rel_path).map(Clone::clone).unwrap_or_default())
    }
}
