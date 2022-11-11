use std::{fmt::Debug, future::Future, io, marker::PhantomData, path::Path, pin::Pin};

use binstalk_manifests::cargo_toml_binstall::{PkgFmtDecomposed, TarBasedFmt};
use digest::{Digest, FixedOutput, HashMarker, Output, OutputSizeUser, Update};
use thiserror::Error as ThisError;
use tracing::debug;

pub use binstalk_manifests::cargo_toml_binstall::PkgFmt;
pub use tar::Entries;
pub use zip::result::ZipError;

use crate::remote::{Client, Error as RemoteError, Url};

mod async_extracter;
pub use async_extracter::TarEntriesVisitor;
use async_extracter::*;

mod extracter;
mod stream_readable;

pub type CancellationFuture = Option<Pin<Box<dyn Future<Output = Result<(), io::Error>> + Send>>>;

#[derive(Debug, ThisError)]
pub enum DownloadError {
    #[error(transparent)]
    Unzip(#[from] ZipError),

    #[error(transparent)]
    Remote(#[from] RemoteError),

    /// A generic I/O error.
    ///
    /// - Code: `binstall::io`
    /// - Exit: 74
    #[error(transparent)]
    Io(io::Error),

    #[error("installation cancelled by user")]
    UserAbort,
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
    pub async fn and_visit_tar<V: TarEntriesVisitor + Debug + Send + 'static>(
        self,
        fmt: TarBasedFmt,
        visitor: V,
        cancellation_future: CancellationFuture,
    ) -> Result<V::Target, DownloadError> {
        let stream = self.client.get_stream(self.url).await?;

        debug!("Downloading and extracting then in-memory processing");

        let ret =
            extract_tar_based_stream_and_visit(stream, fmt, visitor, cancellation_future).await?;

        debug!("Download, extraction and in-memory procession OK");

        Ok(ret)
    }

    /// Download a file from the provided URL and extract it to the provided path.
    ///
    /// `cancellation_future` can be used to cancel the extraction and return
    /// [`DownloadError::UserAbort`] error.
    pub async fn and_extract(
        self,
        fmt: PkgFmt,
        path: impl AsRef<Path>,
        cancellation_future: CancellationFuture,
    ) -> Result<(), DownloadError> {
        let stream = self.client.get_stream(self.url).await?;

        let path = path.as_ref();
        debug!("Downloading and extracting to: '{}'", path.display());

        match fmt.decompose() {
            PkgFmtDecomposed::Tar(fmt) => {
                extract_tar_based_stream(stream, path, fmt, cancellation_future).await?
            }
            PkgFmtDecomposed::Bin => extract_bin(stream, path, cancellation_future).await?,
            PkgFmtDecomposed::Zip => extract_zip(stream, path, cancellation_future).await?,
        }

        debug!("Download OK, extracted to: '{}'", path.display());

        Ok(())
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
