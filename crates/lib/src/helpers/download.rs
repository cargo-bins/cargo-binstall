use std::{fmt::Debug, path::Path, marker::PhantomData};

use digest::{Digest, FixedOutput, Output, Update, HashMarker, OutputSizeUser};
use log::debug;
use reqwest::{Client, Url};

use crate::{
    errors::BinstallError,
    helpers::{
        remote::create_request,
    },
    manifests::cargo_toml_binstall::{PkgFmt, PkgFmtDecomposed, TarBasedFmt},
};

use async_extracter::*;
pub use async_extracter::TarEntriesVisitor;

mod async_extracter;
mod extracter;
mod stream_readable;

#[derive(Debug)]
pub struct Download<'client, D: Digest = NoDigest> {
    client: &'client Client,
    url: Url,
    _digest: PhantomData<D>,
    _checksum: Vec<u8>,
}

impl<'client> Download<'client> {
    pub fn new(client: &'client Client, url: Url) -> Self {
        Self { client, url, _digest: PhantomData::default(), _checksum: Vec::new() }
    }
}

impl<'client, D: Digest> Download<'client, D> {
    pub fn new_with_checksum(client: &'client Client, url: Url, checksum: Vec<u8>) -> Self {
        Self { client, url, _digest: PhantomData::default(), _checksum: checksum }
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

    /// Download a file from the provided URL and extract part of it to
    /// the provided path.
    ///
    ///  * `filter` - If Some, then it will pass the path of the file to it
    ///    and only extract ones which filter returns `true`.
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
