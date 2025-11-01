//! Locked files with the same API as normal [`File`]s.
//!
//! These use the same mechanisms as, and are interoperable with, Cargo.

use std::{
    fs::{File, TryLockError},
    io::{self, IoSlice, IoSliceMut, SeekFrom},
    ops,
    path::Path,
};

use cfg_if::cfg_if;

cfg_if! {
    if #[cfg(any(target_os = "android", target_os = "illumos"))] {
        use fs4::fs_std::FileExt;

        fn lock_exclusive(file: &File) -> io::Result<()> {
            FileExt::lock_exclusive(file)
        }

        fn lock_shared(file: &File) -> io::Result<()> {
            FileExt::lock_shared(file)
        }

        fn map_try_lock_result(result: io::Result<bool>) -> Result<(), TryLockError> {
            match result {
                Ok(true) => Ok(()),
                Ok(false) => Err(TryLockError::WouldBlock),
                Err(e) if e.raw_os_error() == fs4::lock_contended_error().raw_os_error() => {
                    Err(TryLockError::WouldBlock)
                }
                Err(e) => Err(TryLockError::Error(e)),
            }
        }

        fn try_lock_exclusive(file: &File) -> Result<(), TryLockError> {
            map_try_lock_result(FileExt::try_lock_exclusive(file))
        }

        fn try_lock_shared(file: &File) -> Result<(), TryLockError> {
            map_try_lock_result(FileExt::try_lock_shared(file))
        }

        fn unlock(file: &File) -> io::Result<()> {
            FileExt::unlock(file)
        }
    } else {
        fn lock_exclusive(file: &File) -> io::Result<()> {
            file.lock()
        }

        fn lock_shared(file: &File) -> io::Result<()> {
            file.lock_shared()
        }

        fn try_lock_exclusive(file: &File) -> Result<(), TryLockError> {
            file.try_lock()
        }

        fn try_lock_shared(file: &File) -> Result<(), TryLockError> {
            file.try_lock_shared()
        }

        fn unlock(file: &File) -> io::Result<()> {
            file.unlock()
        }
    }
}

/// A locked file.
#[derive(Debug)]
pub struct FileLock(File, #[cfg(feature = "tracing")] Option<Box<Path>>);

impl FileLock {
    #[cfg(not(feature = "tracing"))]
    fn new(file: File) -> Self {
        Self(file)
    }

    #[cfg(feature = "tracing")]
    fn new(file: File) -> Self {
        Self(file, None)
    }

    /// Take an exclusive lock on a [`File`].
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_exclusive(file: File) -> io::Result<Self> {
        lock_exclusive(&file)?;

        Ok(Self::new(file))
    }

    /// Try to take an exclusive lock on a [`File`].
    ///
    /// On success returns [`Self`]. On error the original [`File`] and optionally
    /// an [`io::Error`] if the the failure was caused by anything other than
    /// the lock being taken already.
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_try_exclusive(file: File) -> Result<Self, (File, Option<io::Error>)> {
        match try_lock_exclusive(&file) {
            Ok(()) => Ok(Self::new(file)),
            Err(TryLockError::WouldBlock) => Err((file, None)),
            Err(TryLockError::Error(e)) => Err((file, Some(e))),
        }
    }

    /// Take a shared lock on a [`File`].
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_shared(file: File) -> io::Result<Self> {
        lock_shared(&file)?;

        Ok(Self::new(file))
    }

    /// Try to take a shared lock on a [`File`].
    ///
    /// On success returns [`Self`]. On error the original [`File`] and optionally
    /// an [`io::Error`] if the the failure was caused by anything other than
    /// the lock being taken already.
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_try_shared(file: File) -> Result<Self, (File, Option<io::Error>)> {
        match try_lock_shared(&file) {
            Ok(()) => Ok(Self::new(file)),
            Err(TryLockError::WouldBlock) => Err((file, None)),
            Err(TryLockError::Error(e)) => Err((file, Some(e))),
        }
    }

    /// Set path to the file for logging on unlock error, if feature tracing is enabled
    pub fn set_file_path(mut self, path: impl Into<Box<Path>>) -> Self {
        #[cfg(feature = "tracing")]
        {
            self.1 = Some(path.into());
        }
        self
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _res = unlock(&self.0);

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
