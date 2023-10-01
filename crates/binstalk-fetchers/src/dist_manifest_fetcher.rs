use std::collections::BTreeMap;

use binstalk_downloader::remote::{Client, Response};
use cargo_dist_schema::Format;
use compact_str::CompactString;

use crate::FetchError;

#[derive(Clone, Debug)]
struct Binary {
    /// Key: target, value: artifact
    artifacts: BTreeMap<CompactString, Artifact>,
}

/// An tarball/zip artifact.
#[derive(Clone, Debug)]
struct Artifact {
    /// Filename of artifact on release artifacts,
    /// need to infer the format.
    filename: CompactString,
    /// Path to the executable within the tarbal/zip.
    path_to_exe: CompactString,

    /// Filename of the checksum file.
    checksum_filename: Option<CompactString>,
}

#[derive(Clone, Debug)]
pub(super) enum DistManifest {
    NotSupported(Format),
    /// Key: name of the binary
    Binaries(BTreeMap<CompactString, Binary>),
}

impl DistManifest {
    async fn parse(response: Response) -> Result<Self, FetchError> {
        let manifest: cargo_dist_schema::DistManifest = response.json().await?;
        let format = manifest.format();

        if format.unsupported() {
            return Ok(Self::NotSupported(format));
        }

        todo!()
    }
}

// TODO: Cache `DistManifest` in a new global http cacher for the fetchers
// Also cache the artifacts downloaded and extracted

pub struct GhDistManifest {}
