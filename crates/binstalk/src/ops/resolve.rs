use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    iter, mem,
    path::Path,
    sync::Arc,
};

use cargo_toml::Manifest;
use compact_str::{CompactString, ToCompactString};
use crates_io_api::AsyncClient as CratesIoApiClient;
use itertools::Itertools;
use semver::{Version, VersionReq};
use tokio::task::block_in_place;
use tracing::{debug, info, instrument, warn};

use super::Options;
use crate::{
    bins,
    drivers::fetch_crate_cratesio,
    errors::{BinstallError, VersionParseError},
    fetchers::{Data, Fetcher, TargetData},
    helpers::remote::Client,
    manifests::cargo_toml_binstall::{Meta, PkgMeta, PkgOverride},
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

                warn!(
                    "The package will be downloaded from {}{}",
                    if fetcher.is_third_party() {
                        "third-party source "
                    } else {
                        ""
                    },
                    fetcher.source_name()
                );

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
) -> Result<Resolution, BinstallError> {
    let crate_name_name = crate_name.name.clone();
    let resolution = resolve_inner(&opts, crate_name, curr_version)
        .await
        .map_err(|err| err.crate_context(crate_name_name))?;

    resolution.print(&opts);

    Ok(resolution)
}

async fn resolve_inner(
    opts: &Options,
    crate_name: CrateName,
    curr_version: Option<Version>,
) -> Result<Resolution, BinstallError> {
    info!("Resolving package: '{}'", crate_name);

    let version_req: VersionReq = match (crate_name.version_req, &opts.version_req) {
        (Some(version), None) => version,
        (None, Some(version)) => version.clone(),
        (Some(_), Some(_)) => Err(BinstallError::SuperfluousVersionOption)?,
        (None, None) => VersionReq::STAR,
    };

    let version_req_str = version_req.to_compact_string();

    let Some(package_info) = PackageInfo::resolve(opts,
        crate_name.name,
        curr_version,
        version_req,
        opts.client.clone(),
        &opts.crates_io_api_client).await?
    else {
        return Ok(Resolution::AlreadyUpToDate)
    };

    let desired_targets = opts.desired_targets.get().await;
    let resolvers = &opts.resolvers;

    let mut handles: Vec<(Arc<dyn Fetcher>, _)> =
        Vec::with_capacity(desired_targets.len() * resolvers.len());

    let data = Arc::new(Data {
        name: package_info.name.clone(),
        version: package_info.version_str.clone(),
        repo: package_info.repo.clone(),
    });

    handles.extend(
        desired_targets
            .iter()
            .map(|target| {
                debug!("Building metadata for target: {target}");

                let target_meta = package_info.meta.merge_overrides(
                    iter::once(&opts.cli_overrides).chain(package_info.overrides.get(target)),
                );

                debug!("Found metadata: {target_meta:?}");

                Arc::new(TargetData {
                    target: target.clone(),
                    meta: target_meta,
                })
            })
            .cartesian_product(resolvers)
            .map(|(target_data, f)| {
                let fetcher = f(opts.client.clone(), data.clone(), target_data);
                (fetcher.clone(), fetcher.find())
            }),
    );

    for (fetcher, handle) in handles {
        match handle.flattened_join().await {
            Ok(true) => {
                // Generate temporary binary path
                let bin_path = opts.temp_dir.join(format!(
                    "bin-{}-{}-{}",
                    package_info.name,
                    fetcher.target(),
                    fetcher.fetcher_name()
                ));

                match download_extract_and_verify(
                    fetcher.as_ref(),
                    &bin_path,
                    &package_info,
                    &opts.install_path,
                    opts.no_symlinks,
                )
                .await
                {
                    Ok(bin_files) => {
                        if !bin_files.is_empty() {
                            return Ok(Resolution::Fetch {
                                fetcher,
                                new_version: package_info.version,
                                name: package_info.name,
                                version_req: version_req_str,
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

    if opts.cargo_install_fallback {
        Ok(Resolution::InstallFromSource {
            name: package_info.name,
            version: package_info.version_str,
        })
    } else {
        Err(BinstallError::NoFallbackToCargoInstall)
    }
}

///  * `fetcher` - `fetcher.find()` must have returned `Ok(true)`.
///
/// Can return empty Vec if all `BinFile` is optional and does not exist
/// in the archive downloaded.
async fn download_extract_and_verify(
    fetcher: &dyn Fetcher,
    bin_path: &Path,
    package_info: &PackageInfo,
    install_path: &Path,
    no_symlinks: bool,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    // Download and extract it.
    // If that fails, then ignore this fetcher.
    fetcher.fetch_and_extract(bin_path).await?;

    // Build final metadata
    let meta = fetcher.target_meta();

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

    // Verify that all non-optional bin_files exist
    block_in_place(|| {
        let bin_files = collect_bin_files(
            fetcher,
            package_info,
            meta,
            bin_path,
            install_path,
            no_symlinks,
        )?;

        let name = &package_info.name;

        package_info
            .binaries
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
                            let bin_name = bin.name.as_str();
                            let features = required_features.iter().format(",");
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

fn collect_bin_files(
    fetcher: &dyn Fetcher,
    package_info: &PackageInfo,
    meta: PkgMeta,
    bin_path: &Path,
    install_path: &Path,
    no_symlinks: bool,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    // List files to be installed
    // based on those found via Cargo.toml
    let bin_data = bins::Data {
        name: &package_info.name,
        target: fetcher.target(),
        version: &package_info.version_str,
        repo: package_info.repo.as_deref(),
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
    let bin_files = package_info
        .binaries
        .iter()
        .map(|bin| bins::BinFile::new(&bin_data, bin.name.as_str(), &bin_dir, no_symlinks))
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

struct PackageInfo {
    meta: PkgMeta,
    binaries: Vec<Bin>,
    name: CompactString,
    version_str: CompactString,
    version: Version,
    repo: Option<String>,
    overrides: BTreeMap<String, PkgOverride>,
}

struct Bin {
    name: String,
    required_features: Vec<String>,
}

impl PackageInfo {
    /// Return `None` if already up-to-date.
    async fn resolve(
        opts: &Options,
        name: CompactString,
        curr_version: Option<Version>,
        version_req: VersionReq,
        client: Client,
        crates_io_api_client: &CratesIoApiClient,
    ) -> Result<Option<Self>, BinstallError> {
        // Fetch crate via crates.io, git, or use a local manifest path
        let manifest = match opts.manifest_path.as_ref() {
            Some(manifest_path) => load_manifest_path(manifest_path)?,
            None => fetch_crate_cratesio(client, crates_io_api_client, &name, &version_req).await?,
        };

        let Some(mut package) = manifest.package else {
            return Err(BinstallError::CargoTomlMissingPackage(name));
        };

        let new_version_str = package.version().to_compact_string();
        let new_version = match Version::parse(&new_version_str) {
            Ok(new_version) => new_version,
            Err(err) => {
                return Err(Box::new(VersionParseError {
                    v: new_version_str,
                    err,
                })
                .into())
            }
        };

        if let Some(curr_version) = curr_version {
            if new_version == curr_version {
                info!(
                    "{} v{curr_version} is already installed, use --force to override",
                    name
                );
                return Ok(None);
            }
        }

        let (mut meta, binaries): (_, Vec<Bin>) = (
            package
                .metadata
                .take()
                .and_then(|m| m.binstall)
                .unwrap_or_default(),
            manifest
                .bin
                .into_iter()
                .filter_map(|p| {
                    p.name.map(|name| Bin {
                        name,
                        required_features: p.required_features,
                    })
                })
                .collect(),
        );

        // Check binaries
        if binaries.is_empty() {
            Err(BinstallError::UnspecifiedBinaries)
        } else {
            Ok(Some(Self {
                overrides: mem::take(&mut meta.overrides),
                meta,
                binaries,
                name,
                version_str: new_version_str,
                version: new_version,
                repo: package.repository().map(ToString::to_string),
            }))
        }
    }
}

/// Load binstall metadata from the crate `Cargo.toml` at the provided path
pub fn load_manifest_path<P: AsRef<Path>>(
    manifest_path: P,
) -> Result<Manifest<Meta>, BinstallError> {
    let manifest_path = manifest_path.as_ref();

    block_in_place(|| {
        let manifest_path = if manifest_path.is_dir() {
            Cow::Owned(manifest_path.join("Cargo.toml"))
        } else if manifest_path.is_file() {
            Cow::Borrowed(manifest_path)
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
