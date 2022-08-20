use std::{
    fmt::Debug,
    fs,
    io::{copy, Read, Seek},
    path::Path,
};

use bytes::Bytes;
use futures_util::stream::Stream;
use log::debug;
use scopeguard::{guard, ScopeGuard};
use tar::Entries;
use tempfile::tempfile;
use tokio::task::block_in_place;

use crate::{errors::BinstallError, manifests::cargo_toml_binstall::TarBasedFmt};

use super::{extracter::*, stream_readable::StreamReadable};

pub async fn extract_bin<S, E>(stream: S, path: &Path) -> Result<(), BinstallError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    BinstallError: From<E>,
{
    let mut reader = StreamReadable::new(stream).await;
    block_in_place(move || {
        fs::create_dir_all(path.parent().unwrap())?;

        let mut file = fs::File::create(&path)?;

        // remove it unless the operation isn't aborted and no write
        // fails.
        let remove_guard = guard(&path, |path| {
            fs::remove_file(path).ok();
        });

        copy(&mut reader, &mut file)?;

        // Operation isn't aborted and all writes succeed,
        // disarm the remove_guard.
        ScopeGuard::into_inner(remove_guard);

        Ok(())
    })
}

pub async fn extract_zip<S, E>(stream: S, path: &Path) -> Result<(), BinstallError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    BinstallError: From<E>,
{
    let mut reader = StreamReadable::new(stream).await;
    block_in_place(move || {
        fs::create_dir_all(path.parent().unwrap())?;

        let mut file = tempfile()?;

        copy(&mut reader, &mut file)?;

        // rewind it so that we can pass it to unzip
        file.rewind()?;

        unzip(file, path)
    })
}

pub async fn extract_tar_based_stream<S, E>(
    stream: S,
    path: &Path,
    fmt: TarBasedFmt,
) -> Result<(), BinstallError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    BinstallError: From<E>,
{
    let reader = StreamReadable::new(stream).await;
    block_in_place(move || {
        fs::create_dir_all(path.parent().unwrap())?;

        debug!("Extracting from {fmt} archive to {path:#?}");

        create_tar_decoder(reader, fmt)?.unpack(path)?;

        Ok(())
    })
}

/// Visitor must iterate over all entries.
/// Entires can be in arbitary order.
pub trait TarEntriesVisitor {
    type Target;

    fn visit<R: Read>(&mut self, entries: Entries<'_, R>) -> Result<(), BinstallError>;
    fn finish(self) -> Result<Self::Target, BinstallError>;
}

pub async fn extract_tar_based_stream_and_visit<S, V, E>(
    stream: S,
    fmt: TarBasedFmt,
    mut visitor: V,
) -> Result<V::Target, BinstallError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin + 'static,
    V: TarEntriesVisitor + Debug + Send + 'static,
    BinstallError: From<E>,
{
    let reader = StreamReadable::new(stream).await;
    block_in_place(move || {
        debug!("Extracting from {fmt} archive to process it in memory");

        let mut tar = create_tar_decoder(reader, fmt)?;
        visitor.visit(tar.entries()?)?;
        visitor.finish()
    })
}
