//! Tests for the `Dir` based API.
//!
//! Set `ATOMIC_FILE_INSTALL_TEST_DIR` to a directory on another file system
//! (e.g. a reflink capable btrfs) to also test the copy fallback across file
//! systems.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use atomic_file_install::{
    atomic_install_in, atomic_install_noclobber_in, atomic_symlink_file_in,
    atomic_symlink_file_noclobber_in,
};
use cap_std::{ambient_authority, fs::Dir};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Dir {
    Dir::open_ambient_dir(dir.path(), ambient_authority()).unwrap()
}

fn dest_tempdir() -> TempDir {
    match std::env::var_os("ATOMIC_FILE_INSTALL_TEST_DIR") {
        Some(dir) => tempfile::tempdir_in(dir).unwrap(),
        None => tempfile::tempdir().unwrap(),
    }
}

/// A source dir with an executable `bin` and a destination dir.
fn setup() -> (TempDir, Dir, TempDir, Dir) {
    let src = tempfile::tempdir().unwrap();
    let mut file = fs::File::create(src.path().join("bin")).unwrap();
    file.write_all(b"new").unwrap();
    #[cfg(unix)]
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .unwrap();

    let dst = dest_tempdir();
    let (src_dir, dst_dir) = (open(&src), open(&dst));
    (src, src_dir, dst, dst_dir)
}

/// Only the expected entries exist, no temporary file is left behind.
fn assert_entries(dir: &TempDir, expected: &[&str]) {
    let mut entries: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    entries.sort();
    assert_eq!(entries, expected);
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn install_replaces() {
    let (src, src_dir, dst, dst_dir) = setup();
    fs::write(dst.path().join("bin"), b"old").unwrap();

    atomic_install_in((&src_dir, Path::new("bin")), (&dst_dir, Path::new("bin"))).unwrap();

    assert_eq!(fs::read(dst.path().join("bin")).unwrap(), b"new");
    #[cfg(unix)]
    assert_eq!(mode(&dst.path().join("bin")), 0o755);
    assert_entries(&dst, &["bin"]);
    drop(src);
}

#[test]
fn install_noclobber() {
    let (src, src_dir, dst, dst_dir) = setup();

    atomic_install_noclobber_in((&src_dir, Path::new("bin")), (&dst_dir, Path::new("bin")))
        .unwrap();
    assert_eq!(fs::read(dst.path().join("bin")).unwrap(), b"new");
    #[cfg(unix)]
    assert_eq!(mode(&dst.path().join("bin")), 0o755);
    // The source is copied, not moved.
    assert!(src.path().join("bin").exists());

    let err =
        atomic_install_noclobber_in((&src_dir, Path::new("bin")), (&dst_dir, Path::new("bin")))
            .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
    assert_entries(&dst, &["bin"]);
}

#[test]
fn install_cannot_escape() {
    let (src, src_dir, dst, dst_dir) = setup();
    let outside = dst.path().parent().unwrap().join("outside");

    for dst_path in [Path::new("../outside"), outside.as_path()] {
        atomic_install_in((&src_dir, Path::new("bin")), (&dst_dir, dst_path)).unwrap_err();
        atomic_install_noclobber_in((&src_dir, Path::new("bin")), (&dst_dir, dst_path))
            .unwrap_err();
        assert!(!outside.exists());
    }
    assert!(src.path().join("bin").exists());
    assert_entries(&dst, &[]);
}

/// A source symlink pointing outside of its dir is rejected instead of
/// being followed or moved.
#[cfg(unix)]
#[test]
fn install_src_symlink_outside() {
    let (src, src_dir, dst, dst_dir) = setup();
    let secret = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(secret.path(), src.path().join("link")).unwrap();

    atomic_install_in((&src_dir, Path::new("link")), (&dst_dir, Path::new("bin"))).unwrap_err();
    atomic_install_noclobber_in((&src_dir, Path::new("link")), (&dst_dir, Path::new("bin")))
        .unwrap_err();
    assert_entries(&dst, &[]);
}

/// A source symlink inside its dir is followed: its content is installed,
/// not the symlink.
#[cfg(unix)]
#[test]
fn install_src_symlink_inside() {
    let (src, src_dir, dst, dst_dir) = setup();
    std::os::unix::fs::symlink("bin", src.path().join("link")).unwrap();

    atomic_install_in((&src_dir, Path::new("link")), (&dst_dir, Path::new("bin"))).unwrap();

    let installed = dst.path().join("bin");
    assert!(fs::symlink_metadata(&installed).unwrap().is_file());
    assert_eq!(fs::read(&installed).unwrap(), b"new");
    assert_eq!(mode(&installed), 0o755);
    assert_entries(&dst, &["bin"]);
}

#[test]
fn symlink() {
    let (_src, src_dir, dst, dst_dir) = setup();
    atomic_install_noclobber_in(
        (&src_dir, Path::new("bin")),
        (&dst_dir, Path::new("bin-v1")),
    )
    .unwrap();

    atomic_symlink_file_noclobber_in(Path::new("bin-v1"), (&dst_dir, Path::new("bin"))).unwrap();
    assert_eq!(fs::read(dst.path().join("bin")).unwrap(), b"new");
    let err = atomic_symlink_file_noclobber_in(Path::new("bin-v1"), (&dst_dir, Path::new("bin")))
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");

    // Replace it with a link to another version.
    fs::write(dst.path().join("bin-v2"), b"v2").unwrap();
    atomic_symlink_file_in(Path::new("bin-v2"), (&dst_dir, Path::new("bin"))).unwrap();
    assert_eq!(fs::read(dst.path().join("bin")).unwrap(), b"v2");
    #[cfg(unix)]
    assert_eq!(
        fs::read_link(dst.path().join("bin")).unwrap(),
        Path::new("bin-v2")
    );
    assert_entries(&dst, &["bin", "bin-v1", "bin-v2"]);
}

#[test]
fn symlink_cannot_escape() {
    let (_src, _src_dir, dst, dst_dir) = setup();
    let outside = dst.path().parent().unwrap().join("outside");

    atomic_symlink_file_in(Path::new("bin"), (&dst_dir, Path::new("../outside"))).unwrap_err();
    atomic_symlink_file_noclobber_in(Path::new("bin"), (&dst_dir, Path::new("../outside")))
        .unwrap_err();
    assert!(fs::symlink_metadata(&outside).is_err());
    assert_entries(&dst, &[]);
}
