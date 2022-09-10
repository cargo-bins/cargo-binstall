use std::{fmt::Debug, marker::PhantomData, path::Path};

use digest::{Digest, FixedOutput, HashMarker, Output, OutputSizeUser, Update};
use log::debug;
use reqwest::{Client, Url};

use crate::{
    errors::BinstallError,
    helpers::remote::create_request,
    manifests::cargo_toml_binstall::{PkgFmt, PkgFmtDecomposed, TarBasedFmt},
};

pub use async_extracter::TarEntriesVisitor;
use async_extracter::*;

mod async_extracter;
mod extracter;
mod stream_readable;

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

    /// Download a file from the provided URL and extract part of it to
    /// the provided path.
    ///
    ///  * `filter` - If Some, then it will pass the path of the file to it
    ///    and only extract ones which filter returns `true`.
    ///
    /// This does not support verifying a checksum due to the partial extraction
    /// and will ignore one if specified.
    pub async fn and_visit_tar<V: TarEntriesVisitor + Debug + Send + 'static>(
        self,
        fmt: TarBasedFmt,
        visitor: V,
    ) -> Result<V::Target, BinstallError> {
        let stream = create_request(self.client, self.url).await?;

        debug!("Downloading and extracting then in-memory processing");

        let ret = extract_tar_based_stream_and_visit(stream, fmt, visitor).await?;

        debug!("Download, extraction and in-memory procession OK");

        Ok(ret)
    }

    /// Download a file from the provided URL and extract it to the provided path.
    pub async fn and_extract(
        self,
        fmt: PkgFmt,
        path: impl AsRef<Path>,
    ) -> Result<(), BinstallError> {
        let stream = create_request(self.client, self.url).await?;

        let path = path.as_ref();
        debug!("Downloading and extracting to: '{}'", path.display());

        match fmt.decompose() {
            PkgFmtDecomposed::Tar(fmt) => extract_tar_based_stream(stream, path, fmt).await?,
            PkgFmtDecomposed::Bin => extract_bin(stream, path).await?,
            PkgFmtDecomposed::Zip => extract_zip(stream, path).await?,
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
