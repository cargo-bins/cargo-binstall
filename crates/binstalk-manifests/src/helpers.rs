use std::{fs, io, path::Path};

use fs_lock::FileLock;
use tracing::info;

/// Return exclusively locked file that is readable and writable.
///
/// Blocks until the lock is available, logging once if another process holds
/// it, so that a waiting run does not look stuck.
pub(crate) fn create_if_not_exist(path: &Path) -> io::Result<FileLock> {
    let file = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;

    let file_lock = match FileLock::new_try_exclusive(file) {
        Ok(file_lock) => file_lock,
        Err((_, Some(err))) => return Err(err),
        Err((file, None)) => {
            info!("Blocking waiting for file lock on {}", path.display());
            FileLock::new_exclusive(file)?
        }
    };

    Ok(file_lock.set_file_path(path))
}
