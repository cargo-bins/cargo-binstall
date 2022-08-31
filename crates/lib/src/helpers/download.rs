use std::{fmt::Debug, path::Path};

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
/// Download a file from the provided URL and extract it to the provided path.
pub async fn download_and_extract<P: AsRef<Path>>(
    client: &Client,
    url: &Url,
    fmt: PkgFmt,
    path: P,
) -> Result<(), BinstallError> {
    let stream = create_request(client, url.clone()).await?;

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
pub async fn download_tar_based_and_visit<V: TarEntriesVisitor + Debug + Send + 'static>(
    client: &Client,
    url: Url,
    fmt: TarBasedFmt,
    visitor: V,
) -> Result<V::Target, BinstallError> {
    let stream = create_request(client, url).await?;

    debug!("Downloading and extracting then in-memory processing");

    let ret = extract_tar_based_stream_and_visit(stream, fmt, visitor).await?;

    debug!("Download, extraction and in-memory procession OK");

    Ok(ret)
}
