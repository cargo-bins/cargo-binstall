use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use binstalk_downloader::remote::{Client, Response};
use cargo_dist_schema::Format;
use compact_str::CompactString;
use normalize_path::NormalizePath;

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

impl BinaryArtifact {
    /// Return `Self` and target if the artifact contains binary, or `None`.
    fn new<'artifacts>(
        artifact_id: &str,
        artifacts: &'artifacts BTreeMap<String, cargo_dist_schema::Artifact>,
        app_name: &str,
    ) -> Result<Option<(Self, impl Iterator<Item = &'artifacts String>)>, FetchError> {
        let get_artifact = |artifact_id| {
            artifacts.get(artifact_id).ok_or_else(|| {
                FetchError::InvalidDistManifest(format!("Missing artifact_id {artifact_id}").into())
            })
        };
        let binary_artifact = get_artifact(artifact_id)?;

        if !matches!(
            binary_artifact.kind,
            cargo_dist_schema::ArtifactKind::ExecutableZip
        ) {
            return Ok(None);
        }
        let Some(filename) = binary_artifact.name.as_deref() else {
            return Ok(None);
        };

        let path_to_exe = binary_artifact
            .assets
            .iter()
            .find_map(|asset| {
                match (
                    &asset.kind,
                    asset.path.as_deref().map(|p| Path::new(p).normalize()),
                ) {
                    (&cargo_dist_schema::AssetKind::Executable(_), Some(path))
                        if path.file_name() == Some(OsStr::new(app_name)) =>
                    {
                        Some(path)
                    }

                    _ => None,
                }
            })
            .ok_or_else(|| {
                FetchError::InvalidDistManifest(
                    format!(
                        "Cannot find `{app_name}` in artifact `{filename}` with id `{artifact_id}`"
                    )
                    .into(),
                )
            })?;

        let checksum_filename = if let Some(checksum_artifact_id) = &binary_artifact.checksum {
            let checksum_artifact = get_artifact(checksum_artifact_id)?;

            let Some(checksum_filename) = checksum_artifact.name.as_deref() else {
                return Err(FetchError::InvalidDistManifest(
                    format!("Checksum artifact id {checksum_artifact_id} does not have a filename")
                        .into(),
                ));
            };

            if !matches!(
                binary_artifact.kind,
                cargo_dist_schema::ArtifactKind::Checksum
            ) {
                return Err(FetchError::InvalidDistManifest(
                    format!(
                        "Checksum artifact {checksum_filename} with id {checksum_artifact_id} does not have kind Checksum"
                    )
                    .into(),
                ));
            }

            Some(checksum_filename.into())
        } else {
            None
        };

        Ok(Some((
            Self {
                filename: filename.into(),
                path_to_exe,
                checksum_filename,
            },
            binary_artifact.target_triples.iter(),
        )))
    }
}

#[derive(Clone, Debug)]
struct Binary {
    /// Key: target, value: artifact
    binary_artifacts: BTreeMap<CompactString, BinaryArtifact>,
}

impl Binary {
    fn new(
        artifact_ids: Vec<String>,
        artifacts: &BTreeMap<String, cargo_dist_schema::Artifact>,
        app_name: &str,
    ) -> Result<Self, FetchError> {
        let mut binary_artifacts = BTreeMap::new();

        for artifact_id in artifact_ids {
            if let Some((binary_artifact, targets)) =
                BinaryArtifact::new(&artifact_id, artifacts, app_name)?
            {
                for target in targets {
                    binary_artifacts.insert(target.into(), binary_artifact.clone());
                }
            }
        }
        Ok(Self { binary_artifacts })
    }
}

#[derive(Clone, Debug)]
enum DistManifest {
    NotSupported(Format),
    /// Key: name and version of the binary
    Binaries(BTreeMap<(CompactString, CompactString), Binary>),
}

impl DistManifest {
    async fn parse(response: Response) -> Result<Self, FetchError> {
        let manifest: cargo_dist_schema::DistManifest = response.json().await?;
        let format = manifest.format();

        if format < cargo_dist_schema::Format::Epoch3 {
            return Ok(Self::NotSupported(format));
        }

        let cargo_dist_schema::DistManifest {
            releases,
            artifacts,
            ..
        } = manifest;

        Ok(Self::Binaries(
            releases
                .into_iter()
                .map(|release| {
                    let binary = Binary::new(release.artifacts, &artifacts, &release.app_name)?;
                    Ok::<_, FetchError>((
                        (release.app_name.into(), release.app_version.into()),
                        binary,
                    ))
                })
                .collect::<Result<_, _>>()?,
        ))
    }
}

// TODO: Cache `DistManifest` in a new global http cacher for the fetchers
// Also cache the artifacts downloaded and extracted

pub struct GhDistManifest {}
