use std::{
    fs::{create_dir_all, File},
    io,
    path::Path,
};

use cfg_if::cfg_if;
use rc_zip_sync::{rc_zip::parse::EntryKind, ReadZip};

use super::{DownloadError, ExtractedFiles};

pub(super) fn do_extract_zip(f: File, dir: &Path) -> Result<ExtractedFiles, DownloadError> {
    let mut extracted_files = ExtractedFiles::new();

    for entry in f.read_zip()?.entries() {
        let Some(name) = entry.sanitized_name().map(Path::new) else {
            continue;
        };
        let path = dir.join(name);

        let do_extract_file = || {
            let mut entry_writer = File::create(&path)?;
            let mut entry_reader = entry.reader();
            io::copy(&mut entry_reader, &mut entry_writer)?;

            Ok::<_, io::Error>(())
        };

        let parent = path
            .parent()
            .expect("all full entry paths should have parent paths");
        create_dir_all(parent)?;

        match entry.kind() {
            EntryKind::Symlink => {
                extracted_files.add_file(name);
                cfg_if! {
                    if #[cfg(windows)] {
                        do_extract_file()?;
                    } else {
                        use std::{fs, io::Read};

                        match fs::symlink_metadata(&path) {
                            Ok(metadata) if metadata.is_file() => fs::remove_file(&path)?,
                            _ => (),
                        }

                        let mut src = String::new();
                        entry.reader().read_to_string(&mut src)?;

                        // validate pointing path before creating a symbolic link
                        if src.contains("..") {
                            continue;
                        }
                        std::os::unix::fs::symlink(src, &path)?;
                    }
                }
            }
            EntryKind::Directory => (),
            EntryKind::File => {
                extracted_files.add_file(name);
                do_extract_file()?;
            }
        }
    }

    Ok(extracted_files)
}
