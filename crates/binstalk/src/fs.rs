use std::{fs, io, path::Path};

use tempfile::{NamedTempFile, TempPath};
use tracing::{debug, warn};

fn copy_to_tempfile(src: &Path, dst: &Path) -> io::Result<NamedTempFile> {
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

    Ok(tempfile)
}

/// Install a file.
///
/// This is a blocking function, must be called in `block_in_place` mode.
pub fn atomic_install_noclobber(src: &Path, dst: &Path) -> io::Result<()> {
    debug!(
        "Attempting to rename from '{}' to '{}'.",
        src.display(),
        dst.display()
    );

    let tempfile = copy_to_tempfile(src, dst)?;

    debug!(
        "Persisting '{}' to '{}', fail if dst already exists",
        tempfile.path().display(),
        dst.display()
    );
    tempfile.persist_noclobber(dst)?;

    Ok(())
}

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
        warn!("Attempting at atomic rename failed: {err:#?}, fallback to other methods.");

        #[cfg(target_os = "windows")]
        {
            match win::replace_file(src, dst) {
                Ok(()) => {
                    debug!("ReplaceFileW succeeded.",);
                    return Ok(());
                }
                Err(err) => {
                    warn!("ReplaceFileW failed: {err}, fallback to using tempfile plus rename",);
                }
            }
        }

        // src and dst is not on the same filesystem/mountpoint.
        // Fallback to creating NamedTempFile on the parent dir of
        // dst.

        persist(copy_to_tempfile(src, dst)?.into_temp_path(), dst)?;
    } else {
        debug!("Attempting at atomically succeeded.");
    }

    Ok(())
}

fn symlink_file_inner(dest: &Path, link: &Path) -> io::Result<()> {
    #[cfg(target_family = "unix")]
    std::os::unix::fs::symlink(dest, link)?;

    #[cfg(target_family = "windows")]
    std::os::windows::fs::symlink_file(dest, link)?;

    Ok(())
}

pub fn atomic_symlink_file_noclobber(dest: &Path, link: &Path) -> io::Result<()> {
    match symlink_file_inner(dest, link) {
        Ok(_) => Ok(()),

        #[cfg(target_family = "windows")]
        // Symlinks on Windows are disabled in some editions, so creating one is unreliable.
        // Fallback to copy if it fails.
        Err(_) => atomic_install_noclobber(dest, link),

        #[cfg(not(target_family = "windows"))]
        Err(err) => Err(err),
    }
}

/// Atomically install symlink "link" to a file "dst".
///
/// This is a blocking function, must be called in `block_in_place` mode.
pub fn atomic_symlink_file(dest: &Path, link: &Path) -> io::Result<()> {
    let parent = link.parent().unwrap();

    debug!("Creating tempPath at '{}'", parent.display());
    let temp_path = NamedTempFile::new_in(parent)?.into_temp_path();
    // Remove this file so that we can create a symlink
    // with the name.
    fs::remove_file(&temp_path)?;

    debug!(
        "Creating symlink '{}' to file '{}'",
        temp_path.display(),
        dest.display()
    );

    match symlink_file_inner(dest, &temp_path) {
        Ok(_) => persist(temp_path, link),

        #[cfg(target_family = "windows")]
        // Symlinks on Windows are disabled in some editions, so creating one is unreliable.
        // Fallback to copy if it fails.
        Err(_) => atomic_install(dest, link),

        #[cfg(not(target_family = "windows"))]
        Err(err) => Err(err),
    }
}

fn persist(temp_path: TempPath, to: &Path) -> io::Result<()> {
    debug!("Persisting '{}' to '{}'", temp_path.display(), to.display());
    match temp_path.persist(to) {
        Ok(()) => Ok(()),
        #[cfg(target_os = "windows")]
        Err(tempfile::PathPersistError {
            error,
            path: temp_path,
        }) => {
            warn!(
                "Failed to persist symlink '{}' to '{}': {error}, fallback to ReplaceFileW",
                temp_path.display(),
                to.display(),
            );
            win::replace_file(&temp_path, to).map_err(io::Error::from)
        }
        #[cfg(not(target_os = "windows"))]
        Err(err) => Err(err.into()),
    }
}

#[cfg(target_os = "windows")]
mod win {
    use std::{os::windows::ffi::OsStrExt, path::Path};

    use windows::{
        core::{Error, PCWSTR},
        Win32::Storage::FileSystem::{ReplaceFileW, REPLACE_FILE_FLAGS},
    };

    pub(super) fn replace_file(src: &Path, dst: &Path) -> Result<(), Error> {
        let mut src: Vec<_> = src.as_os_str().encode_wide().collect();
        let mut dst: Vec<_> = dst.as_os_str().encode_wide().collect();

        // Ensure it is terminated with 0
        src.push(0);
        dst.push(0);

        // SAFETY: We use it according its doc
        // https://learn.microsoft.com/en-nz/windows/win32/api/winbase/nf-winbase-replacefilew
        //
        // NOTE that this function is available since windows XP, so we don't need to
        // lazily load this function.
        unsafe {
            ReplaceFileW(
                PCWSTR::from_raw(dst.as_ptr()), // lpreplacedfilename
                PCWSTR::from_raw(src.as_ptr()), // lpreplacementfilename
                PCWSTR::null(),                 // lpbackupfilename, null for no backup file
                REPLACE_FILE_FLAGS(0),          // dwreplaceflags
                None,                           // lpexclude, unused
                None,                           // lpreserved, unused
            )
        }
        .ok()
    }
}
