//! Locked files with the same API as normal [`File`]s.
//!
//! These use the same mechanisms as, and are interoperable with, Cargo.

use std::{
    fs::File,
    io::{self, IoSlice, IoSliceMut, Result, SeekFrom},
    ops,
};

use fs4::FileExt;

/// A locked file.
#[derive(Debug)]
pub struct FileLock(File);

impl FileLock {
    /// Take an exclusive lock on a [`File`].
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_exclusive(file: File) -> Result<Self> {
        file.lock_exclusive()?;

        Ok(Self(file))
    }

    /// Take a shared lock on a [`File`].
    ///
    /// Note that this operation is blocking, and should not be called in async contexts.
    pub fn new_shared(file: File) -> Result<Self> {
        file.lock_shared()?;

        Ok(Self(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.unlock();
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
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> Result<()> {
        self.0.flush()
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> Result<usize> {
        self.0.write_vectored(bufs)
    }
}

impl io::Read for FileLock {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.0.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> Result<usize> {
        self.0.read_vectored(bufs)
    }
}

impl io::Seek for FileLock {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.0.seek(pos)
    }

    fn rewind(&mut self) -> Result<()> {
        self.0.rewind()
    }
    fn stream_position(&mut self) -> Result<u64> {
        self.0.stream_position()
    }
}
