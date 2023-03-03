use std::{
    collections::{hash_map::Entry as HashMapEntry, HashMap, HashSet},
    ffi::OsStr,
    path::Path,
};

#[derive(Debug)]
pub enum ExtractedFilesEntry {
    Dir(Box<HashSet<Box<OsStr>>>),
    File,
}

impl ExtractedFilesEntry {
    fn new_dir(file_name: &OsStr) -> Self {
        ExtractedFilesEntry::Dir(Box::new(HashSet::from([file_name.into()])))
    }
}

#[derive(Debug)]
pub struct ExtractedFiles(HashMap<Box<Path>, ExtractedFilesEntry>);

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
            self.add_dir(parent, path.file_name().unwrap())
        }
    }

    /// * `path` - must be canonical and must not be empty
    ///
    /// NOTE that if the entry for the `path` is previously set to a dir,
    /// it would be replaced with a Dir entry containing `file_name`.
    pub(super) fn add_dir(&mut self, path: &Path, file_name: &OsStr) {
        match self.0.entry(path.into()) {
            HashMapEntry::Vacant(entry) => {
                entry.insert(ExtractedFilesEntry::new_dir(file_name));
            }
            HashMapEntry::Occupied(entry) => match entry.into_mut() {
                ExtractedFilesEntry::Dir(hash_set) => {
                    hash_set.insert(file_name.into());
                }
                entry => *entry = ExtractedFilesEntry::new_dir(file_name),
            },
        }

        self.add_dir_if_has_parent(path);
    }

    pub fn get_entry(&self, path: &Path) -> Option<&ExtractedFilesEntry> {
        self.0.get(path)
    }

    pub fn get_dir(&self, path: &Path) -> Option<&HashSet<Box<OsStr>>> {
        match self.get_entry(path)? {
            ExtractedFilesEntry::Dir(file_names) => Some(file_names),
            ExtractedFilesEntry::File => None,
        }
    }

    pub fn has_file(&self, path: &Path) -> bool {
        matches!(self.get_entry(path), Some(ExtractedFilesEntry::File))
    }
}
