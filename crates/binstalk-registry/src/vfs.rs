use std::{
    collections::{hash_set::HashSet, BTreeMap},
    io,
    path::Path,
};

use cargo_toml_workspace::cargo_toml::AbstractFilesystem;
use normalize_path::NormalizePath;

/// This type stores the filesystem structure for the crate tarball
/// extracted in memory and can be passed to
/// `cargo_toml::Manifest::complete_from_abstract_filesystem`.
#[derive(Debug, Default)]
pub(super) struct Vfs(BTreeMap<Box<Path>, HashSet<Box<str>>>);

impl Vfs {
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
        let rel_path = Path::new(rel_path).normalize();

        Ok(self.0.get(&*rel_path).cloned().unwrap_or_default())
    }
}
