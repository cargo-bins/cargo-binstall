use std::{fs, io, path::Path};

/// Returned file is readable and writable.
pub(crate) fn create_if_not_exist(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::File::options();
    options.read(true).write(true);

    options
        .clone()
        .create_new(true)
        .open(path)
        .or_else(|_| options.open(path))
}
