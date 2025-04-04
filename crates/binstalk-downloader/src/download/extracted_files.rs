use std::{
    collections::{hash_map::Entry as HashMapEntry, HashMap, HashSet},
    ffi::OsStr,
    path::Path,
};

#[derive(Debug)]
#[cfg_attr(test, derive(Eq, PartialEq))]
pub enum ExtractedFilesEntry {
    Dir(Box<HashSet<Box<OsStr>>>),
    File,
}

impl ExtractedFilesEntry {
    fn new_dir(file_name: Option<&OsStr>) -> Self {
        ExtractedFilesEntry::Dir(Box::new(
            file_name
                .map(|file_name| HashSet::from([file_name.into()]))
                .unwrap_or_default(),
        ))
    }
}

#[derive(Debug)]
pub struct ExtractedFiles(pub(super) HashMap<Box<Path>, ExtractedFilesEntry>);

impl ExtractedFiles {
    pub(super) fn new() -> Self {
        Self(Default::default())
    }

    /// * `path` - must be canonical and must not be empty
    ///
    /// NOTE that if the entry for the `path` is previously set to a dir,
    /// it would be replaced with a file.
    pub(super) fn add_file(&mut self, path: &Path) {
        self.0.insert(path.into(), ExtractedFilesEntry::File);
        self.add_dir_if_has_parent(path);
    }

    fn add_dir_if_has_parent(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                self.add_dir_inner(parent, path.file_name());
                self.add_dir_if_has_parent(parent);
            } else {
                self.add_dir_inner(Path::new("."), path.file_name())
            }
        }
    }

    /// * `path` - must be canonical and must not be empty
    ///
    /// NOTE that if the entry for the `path` is previously set to a dir,
    /// it would be replaced with an empty Dir entry.
    pub(super) fn add_dir(&mut self, path: &Path) {
        self.add_dir_inner(path, None);
        self.add_dir_if_has_parent(path);
    }

    /// * `path` - must be canonical and must not be empty
    ///
    /// NOTE that if the entry for the `path` is previously set to a dir,
    /// it would be replaced with a Dir entry containing `file_name` if it
    /// is `Some(..)`, or an empty Dir entry.
    fn add_dir_inner(&mut self, path: &Path, file_name: Option<&OsStr>) {
        match self.0.entry(path.into()) {
            HashMapEntry::Vacant(entry) => {
                entry.insert(ExtractedFilesEntry::new_dir(file_name));
            }
            HashMapEntry::Occupied(entry) => match entry.into_mut() {
                ExtractedFilesEntry::Dir(hash_set) => {
                    if let Some(file_name) = file_name {
                        hash_set.insert(file_name.into());
                    }
                }
                entry => *entry = ExtractedFilesEntry::new_dir(file_name),
            },
        }
    }

    /// * `path` - must be a relative path without `.`, `..`, `/`, `prefix:/`
    ///   and must not be empty, for these values it is guaranteed to
    ///   return `None`.
    ///   But could be set to "." for top-level.
    pub fn get_entry(&self, path: &Path) -> Option<&ExtractedFilesEntry> {
        self.0.get(path)
    }

    /// * `path` - must be a relative path without `.`, `..`, `/`, `prefix:/`
    ///   and must not be empty, for these values it is guaranteed to
    ///   return `None`.
    ///   But could be set to "." for top-level.
    pub fn get_dir(&self, path: &Path) -> Option<&HashSet<Box<OsStr>>> {
        match self.get_entry(path)? {
            ExtractedFilesEntry::Dir(file_names) => Some(file_names),
            ExtractedFilesEntry::File => None,
        }
    }

    /// * `path` - must be a relative path without `.`, `..`, `/`, `prefix:/`
    ///   and must not be empty, for these values it is guaranteed to
    ///   return `false`.
    ///   But could be set to "." for top-level.
    pub fn has_file(&self, path: &Path) -> bool {
        matches!(self.get_entry(path), Some(ExtractedFilesEntry::File))
    }
}
