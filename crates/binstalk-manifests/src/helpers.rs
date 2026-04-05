use std::{fmt, fs, io, ops::Deref, path::Path};

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

#[repr(transparent)]
#[derive(Clone, Default)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T> Deref for Redacted<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}
