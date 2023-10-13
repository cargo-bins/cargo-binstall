use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use binstalk_downloader::remote::{Client, Response};
use cargo_dist_schema::Format;
use compact_str::CompactString;
use normalize_path::NormalizePath;
use tracing::warn;

use crate::FetchError;

/// An tarball/zip artifact.
#[derive(Clone, Debug)]
struct BinaryArtifact {
    /// Filename of artifact on release artifacts,
    /// need to infer the format.
    filename: CompactString,
    /// Path to the executable within the tarbal/zip.
    path_to_exe: PathBuf,

    /// Filename of the checksum file.
    checksum_filename: Option<CompactString>,
}

#[derive(Clone, Debug)]
struct Binary {
    /// Key: target, value: artifact
    binary_artifacts: BTreeMap<CompactString, BinaryArtifact>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct BinaryKey {
    binary_name: CompactString,
    binary_version: CompactString,
    binary_target: CompactString,
}

#[derive(Clone, Debug)]
enum DistManifest {
    NotSupported(Format),
    /// Key: name and version of the binary
    Binaries(BTreeMap<BinaryKey, BinaryArtifact>),
}

impl DistManifest {
    async fn parse(response: Response) -> Result<Self, FetchError> {
        Self::new(response.json().await?)
    }

    fn new(manifest: cargo_dist_schema::DistManifest) -> Result<Self, FetchError> {
        use cargo_dist_schema::{ArtifactKind, Asset};

        let format = manifest.format();

        if format < cargo_dist_schema::Format::Epoch3 {
            return Ok(Self::NotSupported(format));
        }

        let cargo_dist_schema::DistManifest {
            releases,
            artifacts,
            ..
        } = manifest;

        let checksum_artifacts = artifacts
            .iter()
            .filter_map(|(artifact_id, artifact)| {
                match (&artifact.kind, artifact.name.as_deref()) {
                    (&ArtifactKind::Checksum, Some(name)) => {
                        Some((artifact_id.into(), name.into()))
                    }
                    _ => None,
                }
            })
            .collect::<BTreeMap<CompactString, CompactString>>();

        let binary_artifacts = artifacts
            .into_iter()
            .filter_map(|(artifact_id, artifact)| {
                let (ArtifactKind::ExecutableZip, Some(filename)) =
                    (artifact.kind, artifact.name.map(CompactString::from))
                else {
                    return None;
                };

                let checksum_filename = if let Some(checksum_artifact_id) = &artifact.checksum {
                    let checksum_filename = checksum_artifacts.get(&**checksum_artifact_id);

                    if checksum_filename.is_none() {
                        warn!("Missing checksum with artifact_id {artifact_id}");
                    }

                    checksum_filename.cloned()
                } else {
                    None
                };

                Some((
                    CompactString::from(artifact_id),
                    (
                        filename,
                        checksum_filename,
                        artifact.assets,
                        artifact.target_triples,
                    ),
                ))
            })
            .collect::<BTreeMap<
                CompactString,
                (
                    CompactString,
                    Option<CompactString>,
                    Vec<Asset>,
                    Vec<String>,
                ),
            >>();

        let mut binaries = BTreeMap::new();

        for release in releases {
            let app_name = CompactString::from(release.app_name);
            let app_version = CompactString::from(release.app_version);

            for artifact_id in release.artifacts {
                let Some((filename, checksum_filename, assets, targets)) =
                    binary_artifacts.get(&*artifact_id)
                else {
                    continue;
                };

                let Some(path_to_exe) = assets.iter().find_map(|asset| {
                    match (
                        &asset.kind,
                        asset.path.as_deref().map(|p| Path::new(p).normalize()),
                    ) {
                        (&cargo_dist_schema::AssetKind::Executable(_), Some(path))
                            if path.file_name() == Some(OsStr::new(&app_name)) =>
                        {
                            Some(path)
                        }

                        _ => None,
                    }
                }) else {
                    warn!(
                        "Cannot find `{app_name}` in asseets of artifact `{filename}` with id `{artifact_id}`"
                    );
                    continue;
                };

                for target in targets {
                    binaries.insert(
                        BinaryKey {
                            binary_name: app_name.clone(),
                            binary_version: app_version.clone(),
                            binary_target: target.into(),
                        },
                        BinaryArtifact {
                            filename: filename.clone(),
                            checksum_filename: checksum_filename.clone(),
                            path_to_exe: path_to_exe.clone(),
                        },
                    );
                }
            }
        }

        Ok(Self::Binaries(binaries))
    }
}

// TODO: Cache `DistManifest` in a new global http cacher for the fetchers
// Also cache the artifacts downloaded and extracted

pub struct GhDistManifest {}

#[cfg(test)]
mod test {}
