//! Helpers shared by the extractors of archives that need to be read from a
//! seekable file (zip and 7z), which have no extraction routine of their own
//! we can rely on to stay inside the extraction dir.

use std::{fs::create_dir_all, io, path::Path};

pub(super) fn create_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .expect("all full entry paths should have parent paths");
    create_dir_all(parent)
}

/// Create the symlink entry `name` of a `fmt` archive at `path`, reading its
/// target from `target`.
///
/// The symlink is skipped, with a warning, if the target is invalid or would
/// point outside of the extraction dir.
#[cfg(any(unix, target_os = "wasi"))]
pub(super) fn extract_symlink(
    fmt: &str,
    name: &Path,
    path: &Path,
    target: &mut dyn std::io::Read,
    extracted_files: &mut super::ExtractedFiles,
) -> io::Result<()> {
    use std::{ffi::OsStr, io::Read};

    #[cfg(unix)]
    use std::os::unix::{ffi::OsStrExt, fs::symlink};

    #[cfg(target_os = "wasi")]
    use std::os::wasi::{ffi::OsStrExt, fs::symlink_path as symlink};

    use normalize_path::NormalizePath;
    use tracing::warn;

    const MAX_LINK_TARGET: u64 = 4096;

    let mut src = Vec::new();
    target.take(MAX_LINK_TARGET + 1).read_to_end(&mut src)?;
    if src.is_empty() || src.len() as u64 > MAX_LINK_TARGET || src.contains(&0) {
        warn!(
            "Skip {fmt} symlink dest={} with invalid target",
            path.display(),
        );
        return Ok(());
    }

    let src = Path::new(OsStr::from_bytes(&src));
    let Some(src) = &src.try_normalize() else {
        warn!(
            "Skip {fmt} symlink {} => {} with target pointing outside, beware of possible malware",
            src.display(),
            path.display(),
        );
        return Ok(());
    };

    // src is relative to link_dir
    let link_dir = name.parent().unwrap_or(Path::new(""));
    if link_dir.join(src) == *name {
        warn!("Skip symlink loop {}", path.display());
        return Ok(());
    }

    create_parent_dir(path)?;
    symlink(src, path)?;

    extracted_files.add_file(name);

    Ok(())
}
