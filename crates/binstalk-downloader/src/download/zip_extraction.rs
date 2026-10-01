use std::{
    cfg_select,
    fs::{create_dir_all, File},
    io,
    path::Path,
};

use normalize_path::NormalizePath;
use rc_zip_sync::{rc_zip::parse::EntryKind, ReadZip};
use tracing::warn;

use super::{DownloadError, ExtractedFiles};

#[cfg(not(windows))]
fn read_symlink_src(reader: impl io::Read) -> io::Result<std::path::PathBuf> {
    use std::io::Read;
    
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;
    
    #[cfg(target_os = "wasi")]
    use std::os::wasi::ffi::OsStringExt;

    cfg_select! {
        any(unix, target_os = "wasi") => {
            let mut src = Vec::new();
            reader.read_to_end(&mut src)?;
            Ok(OsStringExt::from_vec(src).into())
        }
        _ => {
            let mut src = String::new();
            reader.read_to_string(&mut src)?;
            Ok(src.into())
        }
    }
}

fn create_parent_dir(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .expect("all full entry paths should have parent paths");
    create_dir_all(parent)
}

pub(super) fn do_extract_zip(f: File, dir: &Path) -> Result<ExtractedFiles, DownloadError> {
    let mut extracted_files = ExtractedFiles::new();

    for entry in f.read_zip()?.entries() {
        let Some(name) = entry.sanitized_name() else {
            warn!("Skip zip entry {} for suspected zip slip", entry.name);
            continue;
        };
        let Some(name) = Path::new(name).try_normalize() else {
            warn!("Skip zip entry {name} pointing outside, beware of possible malware");
            continue;
        };
        let path = dir.join(&name);

        match entry.kind() {
            #[cfg(not(windows))]
            EntryKind::Symlink => {
                let src = read_symlink_src(entry.reader())?;

                let Some(src) = src.try_normalize() else {
                    warn!(
                        "Skip zip symlink {} pointing outside, beware of possible malware",
                        src.display(),
                    );
                    continue;
                };
                if src == path {
                    warn!("Skip symlink loop {} -> {}", src.display(), path.display());
                    continue;
                }

                create_parent_dir(&src)?;
                create_parent_dir(&path)?;

                std::os::unix::fs::symlink(src, &path)?;
                extracted_files.add_file(&name);
            }
            EntryKind::Directory => {
                create_dir_all(path)?;
            }
            #[cfg_attr(not(windows), allow(unreachable_patterns))] // Symlink is handled above on non-Windows
            EntryKind::File | EntryKind::Symlink => {
                create_parent_dir(&path)?;

                let mut entry_writer = File::create_new(&path)?;
                let mut entry_reader = entry.reader();
                io::copy(&mut entry_reader, &mut entry_writer)?;

                extracted_files.add_file(&name);
            }
        }
    }

    Ok(extracted_files)
}
