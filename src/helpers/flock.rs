use std::fs::File;
use std::io;
use std::ops;

use fs4::FileExt;

#[derive(Debug)]
pub struct FileLock(File);

impl FileLock {
    /// NOTE that this function blocks, so it cannot
    /// be called in async context.
    pub fn new_exclusive(file: File) -> io::Result<Self> {
        file.lock_exclusive()?;

        Ok(Self(file))
    }

    /// NOTE that this function blocks, so it cannot
    /// be called in async context.
    pub fn new_shared(file: File) -> io::Result<Self> {
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
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }

    fn write_vectored(&mut self, bufs: &[io::IoSlice<'_>]) -> io::Result<usize> {
        self.0.write_vectored(bufs)
    }
}
