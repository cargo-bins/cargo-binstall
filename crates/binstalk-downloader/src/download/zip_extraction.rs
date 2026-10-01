use std::{
    fs::{create_dir_all, File},
    io,
    path::Path,
};

use normalize_path::NormalizePath;
use rc_zip_sync::{rc_zip::parse::EntryKind, ReadZip};
use tracing::warn;

use super::{DownloadError, ExtractedFiles};

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
        let path = &dir.join(&name);

        match entry.kind() {
            #[cfg(any(unix, target_os = "wasi"))]
            EntryKind::Symlink => {
                use std::io::Read;
    
                #[cfg(unix)]
                use std::os::unix::{ffi::OsStrExt, fs::symlink};
    
                #[cfg(target_os = "wasi")]
                use std::os::wasi::{ffi::OsStrExt, fs::symlink_path as symlink};

                let mut src = Vec::new();
                reader.read_to_end(&mut src)?;
                let src = Path::new(OsStr::from_bytes(&src));

                let Some(src) = &src.try_normalize() else {
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

                create_parent_dir(src)?;
                create_parent_dir(path)?;

                symlink(src, path)?;
                extracted_files.add_file(&name);
            }
            EntryKind::Directory => {
                create_dir_all(path)?;
            }
            #[cfg_attr(any(unix, target_os = "wasi"), allow(unreachable_patterns))]
            EntryKind::File | EntryKind::Symlink => {
                create_parent_dir(path)?;

                let mut entry_writer = File::create_new(path)?;
                let mut entry_reader = entry.reader();
                io::copy(&mut entry_reader, &mut entry_writer)?;

                extracted_files.add_file(&name);
            }
        }
    }

    Ok(extracted_files)
}
