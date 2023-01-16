use std::{fs, io, path::Path};

use tempfile::NamedTempFile;
use tracing::{debug, warn};

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
                    warn!(
                        "ReplaceFileW failed: {err}, fallback to using tempfile plus rename",
                        src.display(),
                        dst.display()
                    );
                }
            }
        }

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
