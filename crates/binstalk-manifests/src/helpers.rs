use std::{fmt, fs, io, path::Path};

use fs_lock::FileLock;

/// Return exclusively locked file that is readable and writable.
pub(crate) fn create_if_not_exist(path: &Path) -> io::Result<FileLock> {
    fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .and_then(FileLock::new_exclusive)
        .map(|file_lock| file_lock.set_file_path(path))
}

pub(crate) struct RedactedOption<'a, T>(pub &'a Option<T>);

impl<T> fmt::Debug for RedactedOption<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(_) => f.debug_tuple("Some").field(&"<redacted>").finish(),
            None => f.write_str("None"),
        }
    }
}
