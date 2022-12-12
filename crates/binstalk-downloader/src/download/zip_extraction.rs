use std::{
    io,
    path::{Component, Path, PathBuf},
};

use async_zip::{read::ZipEntryReader, ZipEntryExt};
use thiserror::Error as ThisError;
use tokio::{fs, io::AsyncRead, task::spawn_blocking};

use super::DownloadError;

#[derive(Debug, ThisError)]
enum ZipErrorInner {
    #[error(transparent)]
    Inner(#[from] async_zip::error::ZipError),

    #[error("Invalid file path: {0}")]
    InvalidFilePath(Box<str>),
}

#[derive(Debug, ThisError)]
#[error(transparent)]
pub struct ZipError(#[from] ZipErrorInner);

impl ZipError {
    pub(super) fn from_inner(err: async_zip::error::ZipError) -> Self {
        Self(ZipErrorInner::Inner(err))
    }
}

pub(super) async fn extract_zip_entry<R>(
    entry: ZipEntryReader<'_, R>,
    path: &Path,
) -> Result<(), DownloadError>
where
    R: AsyncRead + Unpin + Send + Sync,
{
    // Sanitize filename
    let raw_filename = entry.entry().filename();
    let filename = check_filename_and_normalize(raw_filename)
        .ok_or_else(|| ZipError(ZipErrorInner::InvalidFilePath(raw_filename.into())))?;

    // Calculates the outpath
    let outpath = path.join(filename);

    // Get permissions
    let mut perms = None;

    #[cfg(unix)]
    {
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};

        if let Some(mode) = entry.entry().unix_permissions() {
            let mode: u16 = mode;
            perms = Some(Permissions::from_mode(mode as u32));
        }
    }

    if raw_filename.ends_with('/') {
        // This entry is a dir.
        asyncify(move || {
            std::fs::create_dir_all(&outpath)?;
            if let Some(perms) = perms {
                std::fs::set_permissions(&outpath, perms)?;
            }

            Ok(())
        })
        .await?;
    } else {
        // This entry is a file.
        let mut outfile = asyncify(move || {
            if let Some(p) = outpath.parent() {
                std::fs::create_dir_all(p)?;
            }
            let outfile = std::fs::File::create(&outpath)?;

            if let Some(perms) = perms {
                outfile.set_permissions(perms)?;
            }

            Ok(outfile)
        })
        .await
        .map(fs::File::from_std)?;

        entry
            .copy_to_end_crc(&mut outfile, 64 * 1024)
            .await
            .map_err(ZipError::from_inner)?;
    }

    Ok(())
}

/// Ensure the file path is safe to use as a [`Path`].
///
/// - It can't contain NULL bytes
/// - It can't resolve to a path outside the current directory
///   > `foo/../bar` is fine, `foo/../../bar` is not.
/// - It can't be an absolute path
///
/// It will then return a normalized path.
///
/// This will read well-formed ZIP files correctly, and is resistant
/// to path-based exploits.
///
/// This function is adapted from `zip::ZipFile::enclosed_name`.
fn check_filename_and_normalize(filename: &str) -> Option<PathBuf> {
    if filename.contains('\0') {
        return None;
    }

    let mut path = PathBuf::new();

    // The following loop is adapted from
    // `normalize_path::NormalizePath::normalize`.
    for component in Path::new(filename).components() {
        match component {
            Component::Prefix(_) | Component::RootDir => return None,
            Component::CurDir => (),
            Component::ParentDir => {
                if !path.pop() {
                    // `PathBuf::pop` returns false if there is no parent.
                    // which means the path is invalid.
                    return None;
                }
            }
            Component::Normal(c) => path.push(c),
        }
    }

    Some(path)
}

/// Copied from tokio https://docs.rs/tokio/latest/src/tokio/fs/mod.rs.html#132
async fn asyncify<F, T>(f: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match spawn_blocking(f).await {
        Ok(res) => res,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::Other,
            "background task failed",
        )),
    }
}
