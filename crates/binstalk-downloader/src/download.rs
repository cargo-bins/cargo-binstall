use std::{fmt, io, path::Path};

use binstalk_types::cargo_toml_binstall::PkgFmtDecomposed;
use bytes::Bytes;
use futures_util::{stream::FusedStream, Stream, StreamExt};
use thiserror::Error as ThisError;
use tracing::{debug, error, instrument};

pub use binstalk_types::cargo_toml_binstall::{PkgFmt, TarBasedFmt};
pub use rc_zip_sync::rc_zip::error::Error as ZipError;

use crate::remote::{Client, Error as RemoteError, Response, Url};

mod async_extractor;
use async_extractor::*;

mod async_tar_visitor;
use async_tar_visitor::extract_tar_based_stream_and_visit;
pub use async_tar_visitor::{TarEntriesVisitor, TarEntry, TarEntryType};

mod extractor;

mod extracted_files;
pub use extracted_files::{ExtractedFiles, ExtractedFilesEntry};

mod zip_extraction;

#[derive(Debug, ThisError)]
#[non_exhaustive]
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
        err.downcast::<DownloadError>()
            .unwrap_or_else(DownloadError::Io)
    }
}

impl From<DownloadError> for io::Error {
    fn from(e: DownloadError) -> io::Error {
        match e {
            DownloadError::Io(io_error) => io_error,
            e => io::Error::other(e),
        }
    }
}

pub trait DataVerifier: Send + Sync {
    /// Digest input data.
    ///
    /// This method can be called repeatedly for use with streaming messages,
    /// it will be called in the order of the message received.
    fn update(&mut self, data: &Bytes);

    /// Finalise the data verification.
    ///
    /// Return false if the data is invalid.
    fn validate(&mut self) -> bool;
}

impl DataVerifier for () {
    fn update(&mut self, _: &Bytes) {}
    fn validate(&mut self) -> bool {
        true
    }
}

#[derive(Debug)]
enum DownloadContent {
    ToIssue { client: Client, url: Url },
    Response(Response),
}

impl DownloadContent {
    async fn into_response(self) -> Result<Response, DownloadError> {
        Ok(match self {
            DownloadContent::ToIssue { client, url } => client.get(url).send(true).await?,
            DownloadContent::Response(response) => response,
        })
    }
}

pub struct Download<'a> {
    content: DownloadContent,
    data_verifier: Option<&'a mut dyn DataVerifier>,
}

impl fmt::Debug for Download<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.content, f)
    }
}

impl Download<'static> {
    pub fn new(client: Client, url: Url) -> Self {
        Self {
            content: DownloadContent::ToIssue { client, url },
            data_verifier: None,
        }
    }

    pub fn from_response(response: Response) -> Self {
        Self {
            content: DownloadContent::Response(response),
            data_verifier: None,
        }
    }
}

impl<'a> Download<'a> {
    pub fn new_with_data_verifier(
        client: Client,
        url: Url,
        data_verifier: &'a mut dyn DataVerifier,
    ) -> Self {
        Self {
            content: DownloadContent::ToIssue { client, url },
            data_verifier: Some(data_verifier),
        }
    }

    pub fn from_response_with_data_verifier(
        response: Response,
        data_verifier: &'a mut dyn DataVerifier,
    ) -> Self {
        Self {
            content: DownloadContent::Response(response),
            data_verifier: Some(data_verifier),
        }
    }

    pub fn with_data_verifier(self, data_verifier: &mut dyn DataVerifier) -> Download<'_> {
        Download {
            content: self.content,
            data_verifier: Some(data_verifier),
        }
    }

    async fn get_stream(
        self,
    ) -> Result<
        impl FusedStream<Item = Result<Bytes, DownloadError>> + Send + Sync + Unpin + 'a,
        DownloadError,
    > {
        let mut data_verifier = self.data_verifier;
        Ok(self
            .content
            .into_response()
            .await?
            .bytes_stream()
            .map(move |res| {
                let bytes = res?;

                if let Some(data_verifier) = &mut data_verifier {
                    data_verifier.update(&bytes);
                }

                Ok(bytes)
            })
            // Call `fuse` at the end to make sure `data_verifier` is only
            // called when the stream still has elements left.
            .fuse())
    }
}

/// Make sure `stream` is an alias instead of taking the value to avoid
/// exploding size of the future generated.
///
/// Accept `FusedStream` only since the `stream` could be already consumed.
async fn consume_stream<S>(stream: &mut S)
where
    S: Stream<Item = Result<Bytes, DownloadError>> + FusedStream + Unpin,
{
    while let Some(res) = stream.next().await {
        if let Err(err) = res {
            error!(?err, "failed to consume stream");
            break;
        }
    }
}

impl Download<'_> {
    /// Download a file from the provided URL and process it in memory.
    ///
    /// This does not support verifying a checksum due to the partial extraction
    /// and will ignore one if specified.
    ///
    /// NOTE that this API does not support gnu extension sparse file unlike
    /// [`Download::and_extract`].
    #[instrument(skip(self, visitor))]
    pub async fn and_visit_tar(
        self,
        fmt: TarBasedFmt,
        visitor: &mut dyn TarEntriesVisitor,
    ) -> Result<(), DownloadError> {
        let has_data_verifier = self.data_verifier.is_some();
        let mut stream = self.get_stream().await?;

        debug!("Downloading and extracting then in-memory processing");

        let res = extract_tar_based_stream_and_visit(&mut stream, fmt, visitor).await;

        if has_data_verifier {
            consume_stream(&mut stream).await;
        }

        if res.is_ok() {
            debug!("Download, extraction and in-memory procession OK");
        }

        res
    }

    /// Download a file from the provided URL and extract it to the provided path.
    ///
    /// NOTE that this will only extract directory and regular files.
    #[instrument(
        skip(self, path),
        fields(path = format_args!("{}", path.as_ref().display()))
    )]
    pub async fn and_extract(
        self,
        fmt: PkgFmt,
        path: impl AsRef<Path>,
    ) -> Result<ExtractedFiles, DownloadError> {
        async fn inner(
            this: Download<'_>,
            fmt: PkgFmt,
            path: &Path,
        ) -> Result<ExtractedFiles, DownloadError> {
            let has_data_verifier = this.data_verifier.is_some();
            let mut stream = this.get_stream().await?;

            debug!("Downloading and extracting to: '{}'", path.display());

            let res = match fmt.decompose() {
                PkgFmtDecomposed::Tar(fmt) => {
                    extract_tar_based_stream(&mut stream, path, fmt).await
                }
                PkgFmtDecomposed::Bin => extract_bin(&mut stream, path).await,
                PkgFmtDecomposed::Zip => extract_zip(&mut stream, path).await,
            };

            if has_data_verifier {
                consume_stream(&mut stream).await;
            }

            if res.is_ok() {
                debug!("Download OK, extracted to: '{}'", path.display());
            }

            res
        }

        inner(self, fmt, path.as_ref()).await
    }

    #[instrument(skip(self))]
    pub async fn into_bytes(self) -> Result<Bytes, DownloadError> {
        let bytes = self.content.into_response().await?.bytes().await?;
        if let Some(verifier) = self.data_verifier {
            verifier.update(&bytes);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use std::{
        collections::{HashMap, HashSet},
        ffi::OsStr,
        num::NonZeroU16,
    };
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_and_extract() {
        let client = crate::remote::Client::new(
            concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
            None,
            NonZeroU16::new(10).unwrap(),
            1.try_into().unwrap(),
            [],
        )
        .unwrap();

        // cargo-binstall
        let cargo_binstall_url = "https://github.com/cargo-bins/cargo-binstall/releases/download/v0.20.1/cargo-binstall-aarch64-unknown-linux-musl.tgz";

        let extracted_files =
            Download::new(client.clone(), Url::parse(cargo_binstall_url).unwrap())
                .and_extract(PkgFmt::Tgz, tempdir().unwrap())
                .await
                .unwrap();

        assert!(extracted_files.has_file(Path::new("cargo-binstall")));
        assert!(!extracted_files.has_file(Path::new("1234")));

        let files = HashSet::from([OsStr::new("cargo-binstall").into()]);
        assert_eq!(extracted_files.get_dir(Path::new(".")).unwrap(), &files);

        assert_eq!(
            extracted_files.0,
            HashMap::from([
                (
                    Path::new("cargo-binstall").into(),
                    ExtractedFilesEntry::File
                ),
                (
                    Path::new(".").into(),
                    ExtractedFilesEntry::Dir(Box::new(files))
                )
            ])
        );

        // cargo-watch
        let cargo_watch_url = "https://github.com/watchexec/cargo-watch/releases/download/v8.4.0/cargo-watch-v8.4.0-aarch64-unknown-linux-gnu.tar.xz";

        let extracted_files = Download::new(client.clone(), Url::parse(cargo_watch_url).unwrap())
            .and_extract(PkgFmt::Txz, tempdir().unwrap())
            .await
            .unwrap();

        let dir = Path::new("cargo-watch-v8.4.0-aarch64-unknown-linux-gnu");

        assert_eq!(
            extracted_files.get_dir(Path::new(".")).unwrap(),
            &HashSet::from([dir.as_os_str().into()])
        );

        assert_eq!(
            extracted_files.get_dir(dir).unwrap(),
            &HashSet::from_iter(
                [
                    "README.md",
                    "LICENSE",
                    "completions",
                    "cargo-watch",
                    "cargo-watch.1"
                ]
                .iter()
                .map(OsStr::new)
                .map(Box::<OsStr>::from)
            ),
        );

        assert_eq!(
            extracted_files.get_dir(&dir.join("completions")).unwrap(),
            &HashSet::from([OsStr::new("zsh").into()]),
        );

        assert!(extracted_files.has_file(&dir.join("cargo-watch")));
        assert!(extracted_files.has_file(&dir.join("cargo-watch.1")));
        assert!(extracted_files.has_file(&dir.join("LICENSE")));
        assert!(extracted_files.has_file(&dir.join("README.md")));

        assert!(!extracted_files.has_file(&dir.join("completions")));
        assert!(!extracted_files.has_file(&dir.join("asdfcqwe")));

        assert!(extracted_files.has_file(&dir.join("completions/zsh")));

        // sccache, tgz and zip
        let sccache_config = [
            ("https://github.com/mozilla/sccache/releases/download/v0.3.3/sccache-v0.3.3-x86_64-pc-windows-msvc.tar.gz", PkgFmt::Tgz),
            ("https://github.com/mozilla/sccache/releases/download/v0.3.3/sccache-v0.3.3-x86_64-pc-windows-msvc.zip", PkgFmt::Zip),
        ];

        for (sccache_url, fmt) in sccache_config {
            let extracted_files = Download::new(client.clone(), Url::parse(sccache_url).unwrap())
                .and_extract(fmt, tempdir().unwrap())
                .await
                .unwrap();

            let dir = Path::new("sccache-v0.3.3-x86_64-pc-windows-msvc");

            assert_eq!(
                extracted_files.get_dir(Path::new(".")).unwrap(),
                &HashSet::from([dir.as_os_str().into()])
            );

            assert_eq!(
                extracted_files.get_dir(dir).unwrap(),
                &HashSet::from_iter(
                    ["README.md", "LICENSE", "sccache.exe"]
                        .iter()
                        .map(OsStr::new)
                        .map(Box::<OsStr>::from)
                ),
            );
        }
    }
}
