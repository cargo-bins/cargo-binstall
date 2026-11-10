use std::fs;

use atomic_file_install::{atomic_symlink_file, atomic_symlink_file_noclobber};

/// The symlink target must still exist afterwards, also when creating the
/// symlink fails and it falls back to copying (e.g. on Windows without the
/// symlink privilege).
#[test]
fn symlink_keeps_target() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("bin-v1");
    fs::write(&dest, b"v1").unwrap();

    let link = dir.path().join("bin");
    atomic_symlink_file_noclobber(&dest, &link).unwrap();
    assert_eq!(fs::read(&link).unwrap(), b"v1");
    assert_eq!(fs::read(&dest).unwrap(), b"v1");

    let dest = dir.path().join("bin-v2");
    fs::write(&dest, b"v2").unwrap();
    atomic_symlink_file(&dest, &link).unwrap();
    assert_eq!(fs::read(&link).unwrap(), b"v2");
    assert_eq!(fs::read(&dest).unwrap(), b"v2");
}
