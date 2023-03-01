use std::{fmt::Debug, io, marker::PhantomData, path::Path};

use binstalk_types::cargo_toml_binstall::PkgFmtDecomposed;
use digest::{Digest, FixedOutput, HashMarker, Output, OutputSizeUser, Update};
use futures_lite::stream::StreamExt;
use thiserror::Error as ThisError;
use tracing::{debug, instrument};

pub use binstalk_types::cargo_toml_binstall::{PkgFmt, TarBasedFmt};

use crate::remote::{Client, Error as RemoteError, Url};

mod async_extracter;
use async_extracter::*;

mod async_tar_visitor;
use async_tar_visitor::extract_tar_based_stream_and_visit;
pub use async_tar_visitor::{TarEntriesVisitor, TarEntry, TarEntryType};

mod extracter;

mod zip_extraction;
pub use zip_extraction::ZipError;

#[derive(Debug, ThisError)]
pub enum DownloadError {
    #[error("Failed to extract zipfile: {0}")]
    Unzip(#[from] ZipError),

    #[error("Failed to download from remote: {0}")]
    Remote(#[from] RemoteError),

    /// A generic I/O error.
    ///
    /// - Code: `binstall::io`
    /// - Exit: 74
    #[error("I/O Error: {0}")]
    Io(io::Error),
}

impl From<io::Error> for DownloadError {
    fn from(err: io::Error) -> Self {
        if err.get_ref().is_some() {
            let kind = err.kind();

            let inner = err
                .into_inner()
                .expect("err.get_ref() returns Some, so err.into_inner() should also return Some");

            inner
                .downcast()
                .map(|b| *b)
                .unwrap_or_else(|err| DownloadError::Io(io::Error::new(kind, err)))
        } else {
            DownloadError::Io(err)
        }
    }
}

impl From<DownloadError> for io::Error {
    fn from(e: DownloadError) -> io::Error {
        match e {
            DownloadError::Io(io_error) => io_error,
            e => io::Error::new(io::ErrorKind::Other, e),
        }
    }
}

#[derive(Debug)]
pub struct Download<D: Digest = NoDigest> {
    client: Client,
    url: Url,
    _digest: PhantomData<D>,
    _checksum: Vec<u8>,
}

impl Download {
    pub fn new(client: Client, url: Url) -> Self {
        Self {
            client,
            url,
            _digest: PhantomData::default(),
            _checksum: Vec::new(),
        }
    }

    /// Download a file from the provided URL and process them in memory.
    ///
    /// This does not support verifying a checksum due to the partial extraction
    /// and will ignore one if specified.
    ///
    /// `cancellation_future` can be used to cancel the extraction and return
    /// [`DownloadError::UserAbort`] error.
    ///
    /// NOTE that this API does not support gnu extension sparse file unlike
    /// [`Download::and_extract`].
    #[instrument(skip(visitor))]
    pub async fn and_visit_tar(
        self,
        fmt: TarBasedFmt,
        visitor: &mut dyn TarEntriesVisitor,
    ) -> Result<(), DownloadError> {
        let stream = self
            .client
            .get_stream(self.url)
            .await?
            .map(|res| res.map_err(DownloadError::from));

        debug!("Downloading and extracting then in-memory processing");

        extract_tar_based_stream_and_visit(stream, fmt, visitor).await?;

        debug!("Download, extraction and in-memory procession OK");

        Ok(())
    }

    /// Download a file from the provided URL and extract it to the provided path.
    ///
    /// `cancellation_future` can be used to cancel the extraction and return
    /// [`DownloadError::UserAbort`] error.
    #[instrument(skip(path))]
    pub async fn and_extract(
        self,
        fmt: PkgFmt,
        path: impl AsRef<Path>,
    ) -> Result<(), DownloadError> {
        async fn inner(this: Download, fmt: PkgFmt, path: &Path) -> Result<(), DownloadError> {
            let stream = this
                .client
                .get_stream(this.url)
                .await?
                .map(|res| res.map_err(DownloadError::from));

            debug!("Downloading and extracting to: '{}'", path.display());

            match fmt.decompose() {
                PkgFmtDecomposed::Tar(fmt) => extract_tar_based_stream(stream, path, fmt).await?,
                PkgFmtDecomposed::Bin => extract_bin(stream, path).await?,
                PkgFmtDecomposed::Zip => extract_zip(stream, path).await?,
            }

            debug!("Download OK, extracted to: '{}'", path.display());

            Ok(())
        }

        inner(self, fmt, path.as_ref()).await
    }
}

impl<D: Digest> Download<D> {
    pub fn new_with_checksum(client: Client, url: Url, checksum: Vec<u8>) -> Self {
        Self {
            client,
            url,
            _digest: PhantomData::default(),
            _checksum: checksum,
        }
    }

    // TODO: implement checking the sum, may involve bringing (parts of) and_extract() back in here
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoDigest;

impl FixedOutput for NoDigest {
    fn finalize_into(self, _out: &mut Output<Self>) {}
}

impl OutputSizeUser for NoDigest {
    type OutputSize = generic_array::typenum::U0;
}

impl Update for NoDigest {
    fn update(&mut self, _data: &[u8]) {}
}

impl HashMarker for NoDigest {}
