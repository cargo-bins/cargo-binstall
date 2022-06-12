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

    /// * `path` - must be canonical, must not be empty.
    pub(super) fn add_path(&mut self, mut path: &Path) {
        while let Some(parent) = path.parent() {
            // Since path has parent, it must have a filename
            let filename = path.file_name().unwrap();

            // `cargo_toml`'s implementation does the same thing.
            // https://docs.rs/cargo_toml/0.11.5/src/cargo_toml/afs.rs.html#24
            let filename = filename.to_string_lossy();

            self.0
                .entry(parent.into())
                .or_insert_with(|| HashSet::with_capacity(4))
                .insert(filename.into());

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

impl AbstractFilesystem for &Vfs {
    fn file_names_in(&self, rel_path: &str) -> io::Result<HashSet<Box<str>>> {
        (*self).file_names_in(rel_path)
    }
}
