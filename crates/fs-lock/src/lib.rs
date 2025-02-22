//! Locked files with the same API as normal [`File`]s.
//!
//! These use the same mechanisms as, and are interoperable with, Cargo.

use std::{
    fs::File,
    io::{self, IoSlice, IoSliceMut, SeekFrom},
    ops,
};

use fs4::fs_std::FileExt;

/// A locked file.
#[derive(Debug)]
pub struct FileLock(File, Option<Box<Path>>);

impl FileLock {
    /// Take an exclusive lock on a [`File`].
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_exclusive(file: File) -> io::Result<Self> {
        FileExt::lock_exclusive(&file)?;

        Ok(Self(file, None))
    }

    /// Try to take an exclusive lock on a [`File`].
    ///
    /// On success returns [`Self`]. On error the original [`File`] and optionally
    /// an [`io::Error`] if the the failure was caused by anything other than
    /// the lock being taken already.
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_try_exclusive(file: File) -> Result<Self, (File, Option<io::Error>)> {
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Self(file, None)),
            Err(e) if e.raw_os_error() == fs4::lock_contended_error().raw_os_error() => {
                Err((file, None))
            }
            Err(e) => Err((file, Some(e))),
        }
    }

    /// Take a shared lock on a [`File`].
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_shared(file: File) -> io::Result<Self> {
        FileExt::lock_shared(&file)?;

        Ok(Self(file, None))
    }

    /// Try to take a shared lock on a [`File`].
    ///
    /// On success returns [`Self`]. On error the original [`File`] and optionally
    /// an [`io::Error`] if the the failure was caused by anything other than
    /// the lock being taken already.
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_try_shared(file: File) -> Result<Self, (File, Option<io::Error>)> {
        match FileExt::try_lock_shared(&file) {
            Ok(()) => Ok(Self(file, None)),
            Err(e) if e.raw_os_error() == fs4::lock_contended_error().raw_os_error() => {
                Err((file, None))
            }
            Err(e) => Err((file, Some(e))),
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _res = FileExt::unlock(&self.0);
        #[cfg(feature = "tracing")]
        if let Err(err) = _res {
            use std::fmt;

            struct OptionalPath<'a>(Option<&'a Path>);
            impl fmt::Display for OptionalPath<'_> {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    if let Some(path) = self.0 {
                        fmt::Display::fmt(&path.display(), f)
                    } else {
                        Ok(())
                    }
                }
            }

            tracing::warn!(
                "Failed to unlock file{}: {err}",
                OptionalPath(self.1.as_deref()),
            );
        }
    }
}

impl ops::Deref for FileLock {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl ops::DerefMut for FileLock {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl io::Write for FileLock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.0.write_vectored(bufs)
    }
}

impl io::Read for FileLock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        self.0.read_vectored(bufs)
    }
}

impl io::Seek for FileLock {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }

    fn rewind(&mut self) -> io::Result<()> {
        self.0.rewind()
    }
    fn stream_position(&mut self) -> io::Result<u64> {
        self.0.stream_position()
    }
}
