use std::{
    borrow::Cow,
    collections::BTreeSet,
    iter, mem,
    path::{Path, PathBuf},
    sync::Arc,
};

use cargo_toml::{Manifest, Package, Product};
use compact_str::{CompactString, ToCompactString};
use itertools::Itertools;
use semver::{Version, VersionReq};
use tokio::task::block_in_place;
use tracing::{debug, info, instrument, warn};

use super::Options;
use crate::{
    bins,
    drivers::fetch_crate_cratesio,
    errors::BinstallError,
    fetchers::{Data, Fetcher},
    helpers::{remote::Client, tasks::AutoAbortJoinHandle},
    manifests::cargo_toml_binstall::{Meta, PkgMeta},
};

mod crate_name;
#[doc(inline)]
pub use crate_name::CrateName;
mod version_ext;
#[doc(inline)]
pub use version_ext::VersionReqExt;

pub enum Resolution {
    Fetch {
        fetcher: Arc<dyn Fetcher>,
        new_version: Version,
        name: CompactString,
        version_req: CompactString,
        bin_files: Vec<bins::BinFile>,
    },
    InstallFromSource {
        name: CompactString,
        version: CompactString,
    },
    AlreadyUpToDate,
}
impl Resolution {
    fn print(&self, opts: &Options) {
        match self {
            Resolution::Fetch {
                fetcher, bin_files, ..
            } => {
                let fetcher_target = fetcher.target();
                // Prompt user for confirmation
                debug!(
                    "Found a binary install source: {} ({fetcher_target})",
                    fetcher.source_name()
                );

                if fetcher.is_third_party() {
                    warn!(
                        "The package will be downloaded from third-party source {}",
                        fetcher.source_name()
                    );
                } else {
                    info!(
                        "The package will be downloaded from {}",
                        fetcher.source_name()
                    );
                }

                info!("This will install the following binaries:");
                for file in bin_files {
                    info!("  - {}", file.preview_bin());
                }

                if !opts.no_symlinks {
                    info!("And create (or update) the following symlinks:");
                    for file in bin_files {
                        info!("  - {}", file.preview_link());
                    }
                }
            }
            Resolution::InstallFromSource { .. } => {
                warn!("The package will be installed from source (with cargo)",)
            }
            Resolution::AlreadyUpToDate => (),
        }
    }
}

#[instrument(skip_all)]
pub async fn resolve(
    opts: Arc<Options>,
    crate_name: CrateName,
    curr_version: Option<Version>,
    temp_dir: Arc<Path>,
    install_path: Arc<Path>,
    client: Client,
    crates_io_api_client: crates_io_api::AsyncClient,
) -> Result<Resolution, BinstallError> {
    let crate_name_name = crate_name.name.clone();
    let resolution = resolve_inner(
        &opts,
        crate_name,
        curr_version,
        temp_dir,
        install_path,
        client,
        crates_io_api_client,
    )
    .await
    .map_err(|err| err.crate_context(crate_name_name))?;

    resolution.print(&opts);

    Ok(resolution)
}

async fn resolve_inner(
    opts: &Options,
    mut crate_name: CrateName,
    curr_version: Option<Version>,
    temp_dir: Arc<Path>,
    install_path: Arc<Path>,
    client: Client,
    crates_io_api_client: crates_io_api::AsyncClient,
) -> Result<Resolution, BinstallError> {
    info!("Resolving package: '{}'", crate_name);

    let version_req: VersionReq = match (crate_name.version_req.take(), &opts.version_req) {
        (Some(version), None) => version,
        (None, Some(version)) => version.clone(),
        (Some(_), Some(_)) => Err(BinstallError::SuperfluousVersionOption)?,
        (None, None) => VersionReq::STAR,
    };

    // Fetch crate via crates.io, git, or use a local manifest path
    // TODO: work out which of these to do based on `opts.name`
    // TODO: support git-based fetches (whole repo name rather than just crate name)
    let manifest = match opts.manifest_path.as_ref() {
        Some(manifest_path) => load_manifest_path(manifest_path)?,
        None => {
            fetch_crate_cratesio(
                client.clone(),
                &crates_io_api_client,
                &crate_name.name,
                &version_req,
            )
            .await?
        }
    };

    let mut package = manifest
        .package
        .ok_or_else(|| BinstallError::CargoTomlMissingPackage(crate_name.name.clone()))?;

    let new_version =
        Version::parse(package.version()).map_err(|err| BinstallError::VersionParse {
            v: package.version().to_compact_string(),
            err,
        })?;

    if let Some(curr_version) = curr_version {
        if new_version == curr_version {
            info!(
                "{} v{curr_version} is already installed, use --force to override",
                crate_name.name
            );
            return Ok(Resolution::AlreadyUpToDate);
        }
    }

    let (mut meta, mut binaries) = (
        package
            .metadata
            .take()
            .and_then(|mut m| m.binstall.take())
            .unwrap_or_default(),
        manifest.bin,
    );

    binaries.retain(|product| product.name.is_some());

    // Check binaries
    if binaries.is_empty() {
        return Err(BinstallError::UnspecifiedBinaries);
    }

    let desired_targets = opts.desired_targets.get().await;

    let mut handles: Vec<(Arc<dyn Fetcher>, _)> = Vec::with_capacity(desired_targets.len() * 2);

    let overrides = mem::take(&mut meta.overrides);

    handles.extend(
        desired_targets
            .iter()
            .map(|target| {
                debug!("Building metadata for target: {target}");

                let target_meta = meta
                    .merge_overrides(iter::once(&opts.cli_overrides).chain(overrides.get(target)));

                debug!("Found metadata: {target_meta:?}");

                Arc::new(Data {
                    name: package.name.clone(),
                    target: target.clone(),
                    version: package.version().to_string(),
                    repo: package.repository().map(ToString::to_string),
                    meta: target_meta,
                })
            })
            .cartesian_product(&opts.resolver)
            .map(|(fetcher_data, f)| {
                let fetcher = f(&client, &fetcher_data);
                (
                    fetcher.clone(),
                    AutoAbortJoinHandle::spawn(async move { fetcher.find().await }),
                )
            }),
    );

    for (fetcher, handle) in handles {
        match handle.flattened_join().await {
            Ok(true) => {
                // Generate temporary binary path
                let bin_path = temp_dir.join(format!(
                    "bin-{}-{}-{}",
                    crate_name.name,
                    fetcher.target(),
                    fetcher.fetcher_name()
                ));

                match download_extract_and_verify(
                    fetcher.as_ref(),
                    &bin_path,
                    &package,
                    &install_path,
                    &binaries,
                    opts.no_symlinks,
                )
                .await
                {
                    Ok(bin_files) => {
                        if !bin_files.is_empty() {
                            return Ok(Resolution::Fetch {
                                fetcher,
                                new_version,
                                name: crate_name.name,
                                version_req: version_req.to_compact_string(),
                                bin_files,
                            });
                        } else {
                            warn!(
                                "Error when checking binaries provided by fetcher {}: \
                                The fetcher does not provide any optional binary",
                                fetcher.source_name(),
                            );
                        }
                    }
                    Err(err) => {
                        if let BinstallError::UserAbort = err {
                            return Err(err);
                        }
                        warn!(
                            "Error while downloading and extracting from fetcher {}: {}",
                            fetcher.source_name(),
                            err
                        );
                    }
                }
            }
            Ok(false) => (),
            Err(err) => {
                warn!(
                    "Error while checking fetcher {}: {}",
                    fetcher.source_name(),
                    err
                );
            }
        }
    }

    Ok(Resolution::InstallFromSource {
        name: crate_name.name,
        version: package.version().to_compact_string(),
    })
}

///  * `fetcher` - `fetcher.find()` must return `Ok(true)`.
///  * `binaries` - must not be empty
async fn download_extract_and_verify(
    fetcher: &dyn Fetcher,
    bin_path: &Path,
    package: &Package<Meta>,
    install_path: &Path,
    binaries: &[Product],
    no_symlinks: bool,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    // Build final metadata
    let meta = fetcher.target_meta();

    // Download and extract it.
    // If that fails, then ignore this fetcher.
    fetcher.fetch_and_extract(bin_path).await?;

    #[cfg(incomplete)]
    {
        // Fetch and check package signature if available
        if let Some(pub_key) = meta.as_ref().map(|m| m.pub_key.clone()).flatten() {
            debug!("Found public key: {pub_key}");

            // Generate signature file URL
            let mut sig_ctx = ctx.clone();
            sig_ctx.format = "sig".to_string();
            let sig_url = sig_ctx.render(&pkg_url)?;

            debug!("Fetching signature file: {sig_url}");

            // Download signature file
            let sig_path = temp_dir.join(format!("{pkg_name}.sig"));
            download(&sig_url, &sig_path).await?;

            // TODO: do the signature check
            unimplemented!()
        } else {
            warn!("No public key found, package signature could not be validated");
        }
    }

    // Verify that all the bin_files exist
    block_in_place(|| {
        let bin_files = collect_bin_files(
            fetcher,
            package,
            meta,
            binaries,
            bin_path.to_path_buf(),
            install_path.to_path_buf(),
            no_symlinks,
        )?;

        let name = &package.name;

        binaries
            .iter()
            .zip(bin_files)
            .filter_map(|(bin, bin_file)| {
                match bin_file.check_source_exists() {
                    Ok(()) => Some(Ok(bin_file)),

                    // This binary is optional
                    Err(err) => {
                        let required_features = &bin.required_features;

                        if required_features.is_empty() {
                            // This bin is not optional, error
                            Some(Err(err))
                        } else {
                            // Optional, print a warning and continue.
                            let bin_name = bin.name.as_deref().unwrap();
                            let features = required_features.join(",");
                            warn!(
                                "When resolving {name} bin {bin_name} is not found. \
                                But since it requies features {features}, this bin is ignored."
                            );
                            None
                        }
                    }
                }
            })
            .collect::<Result<Vec<bins::BinFile>, BinstallError>>()
    })
}

///  * `binaries` - must not be empty
fn collect_bin_files(
    fetcher: &dyn Fetcher,
    package: &Package<Meta>,
    meta: PkgMeta,
    binaries: &[Product],
    bin_path: PathBuf,
    install_path: PathBuf,
    no_symlinks: bool,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    // List files to be installed
    // based on those found via Cargo.toml
    let bin_data = bins::Data {
        name: &package.name,
        target: fetcher.target(),
        version: package.version(),
        repo: package.repository(),
        meta,
        bin_path,
        install_path,
    };

    let bin_dir = bin_data
        .meta
        .bin_dir
        .as_deref()
        .map(Cow::Borrowed)
        .unwrap_or_else(|| bins::infer_bin_dir_template(&bin_data));

    // Create bin_files
    let bin_files = binaries
        .iter()
        .map(|p| bins::BinFile::from_product(&bin_data, p, &bin_dir, no_symlinks))
        .collect::<Result<Vec<_>, BinstallError>>()?;

    let mut source_set = BTreeSet::new();

    for bin in &bin_files {
        if !source_set.insert(&bin.source) {
            return Err(BinstallError::DuplicateSourceFilePath {
                path: bin.source.clone(),
            });
        }
    }

    Ok(bin_files)
}

/// Load binstall metadata from the crate `Cargo.toml` at the provided path
pub fn load_manifest_path<P: AsRef<Path>>(
    manifest_path: P,
) -> Result<Manifest<Meta>, BinstallError> {
    block_in_place(|| {
        let manifest_path = manifest_path.as_ref();
        let manifest_path = if manifest_path.is_dir() {
            manifest_path.join("Cargo.toml")
        } else if manifest_path.is_file() {
            manifest_path.into()
        } else {
            return Err(BinstallError::CargoManifestPath);
        };

        debug!(
            "Reading manifest at local path: {}",
            manifest_path.display()
        );

        // Load and parse manifest (this checks file system for binary output names)
        let manifest = Manifest::<Meta>::from_path_with_metadata(manifest_path)?;

        // Return metadata
        Ok(manifest)
    })
}
