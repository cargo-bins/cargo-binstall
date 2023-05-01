use std::{
    borrow::Cow,
    io::Write,
    path::{Component, Path, PathBuf},
};

use async_zip::{
    base::{read::WithEntry, read::ZipEntryReader},
    ZipString,
};
use bytes::{Bytes, BytesMut};
use futures_lite::future::try_zip as try_join;
use futures_util::io::Take;
use thiserror::Error as ThisError;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::mpsc,
};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt};

use super::{DownloadError, ExtractedFiles};
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
    zip_reader: &mut ZipEntryReader<'_, Take<Compat<R>>, WithEntry<'_>>,
    path: &Path,
    buf: &mut BytesMut,
    extracted_files: &mut ExtractedFiles,
) -> Result<(), DownloadError>
where
    R: AsyncRead + Unpin + Send + Sync,
{
    // Sanitize filename
    let raw_filename = zip_reader.entry().filename();
    let (filename, is_dir) = check_filename_and_normalize(raw_filename)?;

    // Calculates the outpath
    let outpath = path.join(&filename);

    // Get permissions
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut perms = None;

    #[cfg(unix)]
    {
        use std::{fs::Permissions, os::unix::fs::PermissionsExt};

        if let Some(mode) = zip_reader.entry().unix_permissions() {
            // If it is a dir, then it needs to be at least rwx for the current
            // user so that we can create new files, search for existing files
            // and list its contents.
            //
            // If it is a file, then it needs to be at least readable for the
            // current user.
            let mode: u16 = mode | if is_dir { 0o700 } else { 0o400 };
            perms = Some(Permissions::from_mode(mode as u32));
        }
    }

    if is_dir {
        extracted_files.add_dir(&filename);

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
        extracted_files.add_file(&filename);

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

        let read_task = async move {
            // Read everything into `tx`
            copy_file_to_mpsc(zip_reader.compat(), tx, buf).await?;
            // Check crc32 checksum.
            //
            // NOTE that since everything is alread read into the channel,
            // this function should not read any byte into the `Vec` and
            // should return `0`.
            assert_eq!(zip_reader.read_to_end_checked(&mut Vec::new()).await?, 0);
            Ok(())
        };

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
    mut entry_reader: R,
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
fn check_filename_and_normalize(filename: &ZipString) -> Result<(PathBuf, bool), DownloadError> {
    let filename = filename
        .as_str()
        .map(Cow::Borrowed)
        .unwrap_or_else(|_| String::from_utf8_lossy(filename.as_bytes()));

    let bail = |filename: Cow<'_, str>| {
        Err(ZipError(ZipErrorInner::InvalidFilePath(
            filename.into_owned().into(),
        )))
    };

    if filename.contains('\0') {
        return bail(filename)?;
    }

    let mut path = PathBuf::new();

    // The following loop is adapted from
    // `normalize_path::NormalizePath::normalize`.
    for component in Path::new(&*filename).components() {
        match component {
            Component::Prefix(_) | Component::RootDir => return bail(filename)?,
            Component::CurDir => (),
            Component::ParentDir => {
                if !path.pop() {
                    // `PathBuf::pop` returns false if there is no parent.
                    // which means the path is invalid.
                    return bail(filename)?;
                }
            }
            Component::Normal(c) => path.push(c),
        }
    }

    Ok((path, filename.ends_with('/')))
}
