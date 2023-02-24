use std::{
    io::Write,
    path::{Component, Path, PathBuf},
};

use async_zip::read::stream::{Reading, ZipFileReader};
use bytes::{Bytes, BytesMut};
use futures_lite::future::try_zip as try_join;
use thiserror::Error as ThisError;
use tokio::{
    io::{AsyncRead, AsyncReadExt, Take},
    sync::mpsc,
};

use super::DownloadError;
use crate::utils::asyncify;

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
    zip_reader: &mut ZipFileReader<Reading<'_, Take<R>>>,
    path: &Path,
    buf: &mut BytesMut,
) -> Result<(), DownloadError>
where
    R: AsyncRead + Unpin + Send + Sync,
{
    // Sanitize filename
    let raw_filename = zip_reader.entry().filename();
    let filename = check_filename_and_normalize(raw_filename)
        .ok_or_else(|| ZipError(ZipErrorInner::InvalidFilePath(raw_filename.into())))?;

    // Calculates the outpath
    let outpath = path.join(filename);

    // Get permissions
    let mut perms = None;

    #[cfg(unix)]
    {
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};

        if let Some(mode) = zip_reader.entry().unix_permissions() {
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
        // Use channel size = 5 to minimize the waiting time in the extraction task
        let (tx, mut rx) = mpsc::channel::<Bytes>(5);

        // This entry is a file.

        let write_task = asyncify(move || {
            if let Some(p) = outpath.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut outfile = std::fs::File::create(&outpath)?;

            while let Some(bytes) = rx.blocking_recv() {
                outfile.write_all(&bytes)?;
            }

            outfile.flush()?;

            if let Some(perms) = perms {
                outfile.set_permissions(perms)?;
            }

            Ok(())
        });

        let read_task = copy_file_to_mpsc(zip_reader.reader(), tx, buf);

        try_join(
            async move { write_task.await.map_err(From::from) },
            async move {
                read_task
                    .await
                    .map_err(ZipError::from_inner)
                    .map_err(DownloadError::from)
            },
        )
        .await?;
    }

    Ok(())
}

async fn copy_file_to_mpsc<R: AsyncRead>(
    entry_reader: &mut R,
    tx: mpsc::Sender<Bytes>,
    buf: &mut BytesMut,
) -> Result<(), async_zip::error::ZipError>
where
    R: AsyncRead + Unpin + Send + Sync,
{
    // Since BytesMut does not have a max cap, if AsyncReadExt::read_buf returns
    // 0 then it means Eof.
    while entry_reader.read_buf(buf).await? != 0 {
        // Ensure AsyncReadExt::read_buf can read at least 4096B to avoid
        // frequent expensive read syscalls.
        //
        // Performs this reserve before sending the buf over mpsc queue to
        // increase the possibility of reusing the previous allocation.
        //
        // NOTE: `BytesMut` only reuses the previous allocation if it is the
        // only one holds the reference to it, which is either on the first
        // iteration or all the `Bytes` in the mpsc queue has been consumed,
        // written to the file and dropped.
        //
        // Since reading from entry would have to wait for external file I/O,
        // this would give the blocking thread some time to flush `Bytes`
        // out.
        //
        // If all `Bytes` are flushed out, then we can reuse the allocation here.
        buf.reserve(4096);

        if tx.send(buf.split().freeze()).await.is_err() {
            // Same reason as extract_with_blocking_decoder
            break;
        }
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
