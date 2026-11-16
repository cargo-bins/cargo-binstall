//! Probe executables held in sealed, anonymous memory files.

use std::{
    fs::File,
    io::{self, Write},
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
};

use rustix::{
    fs::{MemfdFlags, Mode, OFlags, SealFlags, fcntl_add_seals, memfd_create, open},
    io::Errno,
};

/// An executable stored in a sealed `memfd`, run through its
/// `/proc/self/fd` path.
///
/// Compared with a file in a temporary directory, it has no name on
/// disk, so it can't collide with or be swapped for another file. Its
/// seals stop anyone from changing it before it runs, and a `noexec`
/// `/tmp` doesn't affect it.
pub(super) struct SealedExecutable {
    path: PathBuf,
    // Keeps the memfd alive until the probe has been spawned.
    _fd: OwnedFd,
}

impl SealedExecutable {
    pub(super) fn new(name: &str, contents: &[u8]) -> io::Result<Self> {
        let flags = MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING;
        // Ask for an executable memfd explicitly: with
        // `vm.memfd_noexec = 1`, a memfd created without `MFD_EXEC` is
        // sealed non-executable. Kernels before 6.3 don't know the flag
        // and reject it with EINVAL, but their memfds are always
        // executable. With `vm.memfd_noexec = 2` both calls fail, and the
        // caller falls back to a temporary directory.
        let fd = match memfd_create(name, flags | MemfdFlags::EXEC) {
            Err(Errno::INVAL) => memfd_create(name, flags),
            res => res,
        }?;
        let mut file = File::from(fd);
        file.write_all(contents)?;

        // From here on the contents and size can't change. `F_SEAL_EXEC`
        // (Linux 6.3) also freezes the mode bits; older kernels reject it
        // with EINVAL.
        let seals = SealFlags::SEAL | SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE;
        match fcntl_add_seals(&file, seals | SealFlags::EXEC) {
            Err(Errno::INVAL) => fcntl_add_seals(&file, seals),
            res => res,
        }?;

        // Reopen read-only and close the writable fd: some kernels refuse
        // to execute a file that is open for writing (ETXTBSY). The probe
        // runs through this same `/proc` path, so this also checks that
        // `/proc` is mounted, and a later ENOENT can only mean a missing
        // loader.
        let fd = open(
            fd_path(&file),
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        drop(file);

        Ok(Self {
            path: fd_path(&fd),
            _fd: fd,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

fn fd_path(fd: &impl AsRawFd) -> PathBuf {
    format!("/proc/self/fd/{}", fd.as_raw_fd()).into()
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;

    use super::*;

    #[test]
    fn sealed_executable_cannot_be_modified() {
        let exe = SealedExecutable::new("test", b"\x7fELF").unwrap();
        assert_eq!(std::fs::read(exe.path()).unwrap(), b"\x7fELF");

        // Reopening for writing works, but the seals reject the write.
        let mut file = OpenOptions::new().write(true).open(exe.path()).unwrap();
        let err = file.write_all(b"x").unwrap_err();
        assert_eq!(err.raw_os_error(), Some(Errno::PERM.raw_os_error()));

        let err = file.set_len(0).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(Errno::PERM.raw_os_error()));
    }
}
