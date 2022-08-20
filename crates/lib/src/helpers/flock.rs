use std::{fs::File, io, ops};

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

impl io::Read for FileLock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [io::IoSliceMut<'_>]) -> io::Result<usize> {
        self.0.read_vectored(bufs)
    }
}

impl io::Seek for FileLock {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }

    fn rewind(&mut self) -> io::Result<()> {
        self.0.rewind()
    }
    fn stream_position(&mut self) -> io::Result<u64> {
        self.0.stream_position()
    }
}
