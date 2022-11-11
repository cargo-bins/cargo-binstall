use std::{fs, io, path::Path};

use log::debug;
use tempfile::NamedTempFile;

/// Atomically install a file.
///
/// This is a blocking function, must be called in `block_in_place` mode.
pub fn atomic_install(src: &Path, dst: &Path) -> io::Result<()> {
    debug!(
        "Attempting to atomically rename from '{}' to '{}'",
        src.display(),
        dst.display()
    );

    if let Err(err) = fs::rename(src, dst) {
        debug!("Attempting at atomic rename failed: {err:#?}, fallback to creating tempfile.");
        // src and dst is not on the same filesystem/mountpoint.
        // Fallback to creating NamedTempFile on the parent dir of
        // dst.

        let mut src_file = fs::File::open(src)?;

        let parent = dst.parent().unwrap();
        debug!("Creating named tempfile at '{}'", parent.display());
        let mut tempfile = NamedTempFile::new_in(parent)?;

        debug!(
            "Copying from '{}' to '{}'",
            src.display(),
            tempfile.path().display()
        );
        io::copy(&mut src_file, tempfile.as_file_mut())?;

        debug!("Retrieving permissions of '{}'", src.display());
        let permissions = src_file.metadata()?.permissions();

        debug!(
            "Setting permissions of '{}' to '{permissions:#?}'",
            tempfile.path().display()
        );
        tempfile.as_file().set_permissions(permissions)?;

        debug!(
            "Persisting '{}' to '{}'",
            tempfile.path().display(),
            dst.display()
        );
        tempfile.persist(dst).map_err(io::Error::from)?;
    } else {
        debug!("Attempting at atomically succeeded.");
    }

    Ok(())
}

fn symlink_file<P: AsRef<Path>, Q: AsRef<Path>>(original: P, link: Q) -> io::Result<()> {
    #[cfg(target_family = "unix")]
    let f = std::os::unix::fs::symlink;
    #[cfg(target_family = "windows")]
    let f = std::os::windows::fs::symlink_file;

    f(original, link)
}

/// Atomically install symlink "link" to a file "dst".
///
/// This is a blocking function, must be called in `block_in_place` mode.
pub fn atomic_symlink_file(dest: &Path, link: &Path) -> io::Result<()> {
    let parent = link.parent().unwrap();

    debug!("Creating tempPath at '{}'", parent.display());
    let temp_path = NamedTempFile::new_in(parent)?.into_temp_path();
    fs::remove_file(&temp_path)?;

    debug!(
        "Creating symlink '{}' to file '{}'",
        temp_path.display(),
        dest.display()
    );
    symlink_file(dest, &temp_path)?;

    debug!(
        "Persisting '{}' to '{}'",
        temp_path.display(),
        link.display()
    );
    temp_path.persist(link).map_err(io::Error::from)
}
