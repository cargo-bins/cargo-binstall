use std::{fs, io, path::Path};

use fs_lock::FileLock;

/// Return exclusively locked file that is readable and writable.
pub(crate) fn create_if_not_exist(path: &Path) -> io::Result<FileLock> {
    let mut options = fs::File::options();
    options.read(true).write(true);

    options
        .clone()
        .create_new(true)
        .open(path)
        .or_else(|_| options.open(path))
        .and_then(FileLock::new_exclusive)
        .map(|file_lock| file_lock.set_file_path(path))
}
