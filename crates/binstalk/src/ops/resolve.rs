use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use cargo_toml::{Manifest, Package, Product};
use compact_str::{CompactString, ToCompactString};
use log::{debug, info, warn};
use reqwest::Client;
use semver::{Version, VersionReq};
use tokio::task::block_in_place;

use super::Options;
use crate::{
    bins,
    drivers::fetch_crate_cratesio,
    errors::BinstallError,
    fetchers::{Data, Fetcher, GhCrateMeta, QuickInstall},
    helpers::tasks::AutoAbortJoinHandle,
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
        package: Arc<Package<Meta>>,
        name: CompactString,
        version_req: CompactString,
        bin_files: Vec<bins::BinFile>,
    },
    InstallFromSource {
        package: Arc<Package<Meta>>,
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
        opts.clone(),
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
    opts: Arc<Options>,
    crate_name: CrateName,
    curr_version: Option<Version>,
    temp_dir: Arc<Path>,
    install_path: Arc<Path>,
    client: Client,
    crates_io_api_client: crates_io_api::AsyncClient,
) -> Result<Resolution, BinstallError> {
    info!("Resolving package: '{}'", crate_name);

    let version_req: VersionReq = match (&crate_name.version_req, &opts.version_req) {
        (Some(version), None) => version.clone(),
        (None, Some(version)) => version.clone(),
        (Some(_), Some(_)) => Err(BinstallError::SuperfluousVersionOption)?,
        (None, None) => VersionReq::STAR,
    };

    // Fetch crate via crates.io, git, or use a local manifest path
    // TODO: work out which of these to do based on `opts.name`
    // TODO: support git-based fetches (whole repo name rather than just crate name)
    let manifest = match opts.manifest_path.clone() {
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

    let package = Arc::new(manifest.package.unwrap());

    if let Some(curr_version) = curr_version {
        let new_version =
            Version::parse(&package.version).map_err(|err| BinstallError::VersionParse {
                v: package.version.clone(),
                err,
            })?;

        if new_version == curr_version {
            info!(
                "{} v{curr_version} is already installed, use --force to override",
                crate_name.name
            );
            return Ok(Resolution::AlreadyUpToDate);
        }
    }

    let (meta, binaries) = (
        package
            .metadata
            .as_ref()
            .and_then(|m| m.binstall.clone())
            .unwrap_or_default(),
        manifest.bin,
    );

    let desired_targets = opts.desired_targets.get().await;

    type JoinHandler = AutoAbortJoinHandle<Result<Option<Vec<bins::BinFile>>, BinstallError>>;

    let mut handles: Vec<(Arc<dyn Fetcher>, JoinHandler)> =
        Vec::with_capacity(desired_targets.len() * 2);

    for target in desired_targets {
        debug!("Building metadata for target: {target}");
        let mut target_meta = meta.clone_without_overrides();

        // Merge any overrides
        if let Some(o) = meta.overrides.get(target) {
            target_meta.merge(o);
        }

        target_meta.merge(&opts.cli_overrides);
        debug!("Found metadata: {target_meta:?}");

        let fetcher_data = Arc::new(Data {
            name: package.name.clone(),
            target: target.clone(),
            version: package.version.clone(),
            repo: package.repository.clone(),
            meta: target_meta,
        });

        // Generate temporary binary path
        let bin_path = temp_dir.join(format!("bin-{}-{target}", crate_name.name));

        handles.extend(
            [
                GhCrateMeta::new(&client, &fetcher_data).await as Arc<dyn Fetcher>,
                QuickInstall::new(&client, &fetcher_data).await as Arc<dyn Fetcher>,
            ]
            .map(|fetcher| {
                let bin_path = bin_path.join(&*fetcher.fetcher_name());
                let package = Arc::clone(&package);
                let install_path = Arc::clone(&install_path);
                let binaries = binaries.clone();

                (
                    fetcher.clone(),
                    AutoAbortJoinHandle::spawn(async move {
                        // Verify that this fetcher contains the package
                        // we want.
                        if !fetcher.find().await? {
                            return Ok(None);
                        }

                        // Build final metadata
                        let meta = fetcher.target_meta();

                        let bin_files = collect_bin_files(
                            fetcher.as_ref(),
                            &package,
                            meta,
                            binaries,
                            bin_path.clone(),
                            install_path.to_path_buf(),
                        )?;

                        // Download and extract it.
                        // If that fails, then ignore this fetcher.
                        fetcher.fetch_and_extract(&bin_path).await?;

                        #[cfg(incomplete)]
                        {
                            // Fetch and check package signature if available
                            if let Some(pub_key) =
                                meta.as_ref().map(|m| m.pub_key.clone()).flatten()
                            {
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
                                warn!(
                                    "No public key found, package signature could not be validated"
                                );
                            }
                        }

                        // Verify that all the bin_files exist
                        block_in_place(|| {
                            for bin_file in bin_files.iter() {
                                bin_file.check_source_exists()?;
                            }

                            Ok(Some(bin_files))
                        })
                    }),
                )
            }),
        );
    }

    for (fetcher, handle) in handles {
        match handle.flattened_join().await {
            Ok(Some(bin_files)) => {
                return Ok(Resolution::Fetch {
                    fetcher,
                    package,
                    name: crate_name.name,
                    version_req: version_req.to_compact_string(),
                    bin_files,
                })
            }
            Ok(None) => (),
            Err(err) => {
                warn!(
                    "Error while checking fetcher {}: {}",
                    fetcher.source_name(),
                    err
                );
            }
        }
    }

    Ok(Resolution::InstallFromSource { package })
}

fn collect_bin_files(
    fetcher: &dyn Fetcher,
    package: &Package<Meta>,
    mut meta: PkgMeta,
    binaries: Vec<Product>,
    bin_path: PathBuf,
    install_path: PathBuf,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    // Update meta
    if fetcher.source_name() == "QuickInstall" {
        // TODO: less of a hack?
        meta.bin_dir = "{ bin }{ binary-ext }".to_string();
    }

    // Check binaries
    if binaries.is_empty() {
        return Err(BinstallError::UnspecifiedBinaries);
    }

    // List files to be installed
    // based on those found via Cargo.toml
    let bin_data = bins::Data {
        name: package.name.clone(),
        target: fetcher.target().to_string(),
        version: package.version.clone(),
        repo: package.repository.clone(),
        meta,
        bin_path,
        install_path,
    };

    // Create bin_files
    let bin_files = binaries
        .iter()
        .map(|p| bins::BinFile::from_product(&bin_data, p))
        .collect::<Result<Vec<_>, BinstallError>>()?;

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
