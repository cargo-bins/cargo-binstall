//! Same as the path based API, except that every path is resolved by
//! [`cap_std`] relative to a [`Dir`] and can never escape it: absolute
//! paths, `..` going above the `Dir` and symlinks pointing outside of it
//! are rejected.

use std::{
    io,
    path::{Path, PathBuf},
};

use cap_std::fs::{Dir, OpenOptions};
use reflink_copy::reflink_file_to_dir;
use tracing::{debug, warn};

/// A file or symlink at a temporary name, removed on drop unless persisted.
struct TempEntry<'a> {
    dir: &'a Dir,
    /// `None` once persisted.
    path: Option<PathBuf>,
}

impl TempEntry<'_> {
    fn path(&self) -> &Path {
        self.path.as_deref().unwrap()
    }

    /// Atomically move it to `to`, replacing `to` if it exists.
    fn persist(mut self, to: &Path) -> io::Result<()> {
        debug!(
            "Persisting '{}' to '{}'",
            self.path().display(),
            to.display()
        );
        self.dir.rename(self.path(), self.dir, to)?;
        self.path = None;
        Ok(())
    }

    /// Move it to `to`, failing if `to` already exists.
    fn persist_noclobber(self, to: &Path) -> io::Result<()> {
        debug!(
            "Persisting '{}' to '{}', fail if dst already exists",
            self.path().display(),
            to.display()
        );
        // Dropping `self` removes the temporary name.
        self.dir.hard_link(self.path(), self.dir, to)
    }
}

impl Drop for TempEntry<'_> {
    fn drop(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(err) = self.dir.remove_file(path) {
            warn!(
                "Failed to remove temporary file '{}': {err}",
                path.display()
            );
        }
    }
}

/// Create an entry at a new temporary name next to `name` in `dir` with
/// `create`, which must fail with [`io::ErrorKind::AlreadyExists`] if the
/// temporary name is already taken, and must not leave anything behind on
/// failure.
fn create_temp<'a>(
    dir: &'a Dir,
    name: &Path,
    mut create: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<TempEntry<'a>> {
    const NUM_RETRIES: u32 = 16;

    let file_name = name.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("`{}` does not have a file name", name.display()),
        )
    })?;

    for _ in 0..NUM_RETRIES {
        let mut temp_name = std::ffi::OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".{:016x}.tmp", fastrand::u64(..)));
        let path = name.with_file_name(temp_name);

        debug!("Creating temporary file '{}'", path.display());
        match create(&path) {
            Ok(()) => {
                return Ok(TempEntry {
                    dir,
                    path: Some(path),
                })
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many temporary files exist",
    ))
}

/// Copy `src` to a temporary file next to `dst`, with the same permissions.
fn copy_to_temp<'a>(
    src: (&Dir, &Path),
    (dst_dir, dst): (&'a Dir, &Path),
) -> io::Result<TempEntry<'a>> {
    debug!("Opening '{}'", src.1.display());
    let src = src.0.open(src.1)?.into_std();

    create_temp(dst_dir, dst, |temp| {
        // src and dst is likely to be on the same filesystem.
        // Uses reflink if the fs support it, or fallback to
        // copying if it doesn't support it or it is not on the
        // same filesystem.
        match reflink_file_to_dir(&src, dst_dir, temp) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Err(err),
            Err(err) => debug!("Failed to reflink: {err}, fallback to copying"),
        }

        let mut dest = dst_dir
            .open_with(temp, OpenOptions::new().write(true).create_new(true))?
            .into_std();
        let res = io::copy(&mut &src, &mut dest)
            .and_then(|_| dest.set_permissions(src.metadata()?.permissions()));
        if res.is_err() {
            drop(dest);
            if let Err(err) = dst_dir.remove_file(temp) {
                warn!(
                    "Failed to remove temporary file '{}': {err}",
                    temp.display()
                );
            }
        }
        res
    })
}

/// Install a file, this fails if the `dst` already exists.
///
/// This is a blocking function, must be called in `block_in_place` mode.
pub fn atomic_install_noclobber_in(src: (&Dir, &Path), dst: (&Dir, &Path)) -> io::Result<()> {
    debug!(
        "Attempting to install from '{}' to '{}', fail if dst already exists",
        src.1.display(),
        dst.1.display()
    );

    copy_to_temp(src, dst)?.persist_noclobber(dst.1)
}

/// Atomically install a file, this atomically replace `dst` if it exists.
///
/// If `src` is a regular file, it is moved to `dst` when possible.
///
/// This is a blocking function, must be called in `block_in_place` mode.
pub fn atomic_install_in(src: (&Dir, &Path), dst: (&Dir, &Path)) -> io::Result<()> {
    debug!(
        "Attempting to atomically rename from '{}' to '{}'",
        src.1.display(),
        dst.1.display()
    );

    // Moving a symlink would install the symlink itself, which may dangle or
    // point outside of `src.0`, so only move regular files.
    if src.0.symlink_metadata(src.1)?.is_file() {
        match src.0.rename(src.1, dst.0, dst.1) {
            Ok(()) => {
                debug!("Attempting at atomically succeeded.");
                return Ok(());
            }
            Err(err) => {
                warn!("Attempting at atomic rename failed: {err}, fallback to other methods.")
            }
        }
    }

    // src and dst is not on the same filesystem/mountpoint.
    // Fallback to creating a temporary file next to dst.
    copy_to_temp(src, dst)?.persist(dst.1)
}

#[cfg(unix)]
fn symlink_file(dir: &Dir, target: &Path, link: &Path) -> io::Result<()> {
    dir.symlink(target, link)
}

#[cfg(windows)]
fn symlink_file(dir: &Dir, target: &Path, link: &Path) -> io::Result<()> {
    dir.symlink_file(target, link)
}

/// Path of the symlink target `target` of `link`, relative to `dir`.
#[cfg(windows)]
fn resolve_target(target: &Path, link: &Path) -> PathBuf {
    link.parent().unwrap_or(Path::new("")).join(target)
}

/// Create a symlink at `link` to `target`, this fails if the `link`
/// already exists.
///
/// `target` is resolved relative to the directory containing `link`, and
/// must be relative.
///
/// This is a blocking function, must be called in `block_in_place` mode.
pub fn atomic_symlink_file_noclobber_in(
    target: &Path,
    (dir, link): (&Dir, &Path),
) -> io::Result<()> {
    match symlink_file(dir, target, link) {
        Ok(()) => Ok(()),

        #[cfg(windows)]
        // Symlinks on Windows are disabled in some editions, so creating one is unreliable.
        // Fallback to copy if it fails.
        Err(err) if err.kind() != io::ErrorKind::AlreadyExists => {
            warn!("Failed to create symlink: {err}, fallback to copying");
            atomic_install_noclobber_in((dir, &resolve_target(target, link)), (dir, link))
        }

        Err(err) => Err(err),
    }
}

/// Atomically create a symlink at `link` to `target`, this atomically replace
/// `link` if it already exists.
///
/// `target` is resolved relative to the directory containing `link`, and
/// must be relative.
///
/// This is a blocking function, must be called in `block_in_place` mode.
pub fn atomic_symlink_file_in(target: &Path, (dir, link): (&Dir, &Path)) -> io::Result<()> {
    debug!(
        "Creating symlink '{}' to file '{}'",
        link.display(),
        target.display()
    );

    match create_temp(dir, link, |temp| symlink_file(dir, target, temp)) {
        Ok(temp) => temp.persist(link),

        #[cfg(windows)]
        // Symlinks on Windows are disabled in some editions, so creating one is unreliable.
        // Fallback to copy if it fails.
        Err(err) => {
            warn!("Failed to create symlink: {err}, fallback to copying");
            // Copy instead of `atomic_install_in`, which would move `target`.
            copy_to_temp((dir, &resolve_target(target, link)), (dir, link))?.persist(link)
        }

        #[cfg(not(windows))]
        Err(err) => Err(err),
    }
}
