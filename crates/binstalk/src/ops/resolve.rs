use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    iter, mem,
    path::Path,
    str::FromStr,
    sync::Arc,
};

use binstalk_fetchers::FETCHER_GH_CRATE_META;
use binstalk_types::{
    cargo_toml_binstall::Strategy,
    crate_info::{CrateSource, SourceType},
};
use compact_str::{CompactString, ToCompactString};
use itertools::Itertools;
use leon::Template;
use maybe_owned::MaybeOwned;
use semver::{Version, VersionReq};
use tokio::{task::spawn_blocking, time::timeout};
use tracing::{debug, error, info, instrument, warn};
use url::Url;

use crate::{
    bins,
    errors::{BinstallError, VersionParseError},
    fetchers::{Data, Fetcher, TargetData},
    helpers::{
        cargo_toml::Manifest, cargo_toml_workspace::load_manifest_from_workspace,
        download::ExtractedFiles, remote::Client, target_triple::TargetTriple,
        tasks::AutoAbortJoinHandle,
    },
    manifests::cargo_toml_binstall::{Meta, PkgMeta, PkgOverride},
    ops::{CargoTomlFetchOverride, Options},
};

mod crate_name;
#[doc(inline)]
pub use crate_name::CrateName;

mod version_ext;
#[doc(inline)]
pub use version_ext::VersionReqExt;

mod resolution;
#[doc(inline)]
pub use resolution::{Resolution, ResolutionFetch, ResolutionSource};

#[instrument(skip_all)]
pub async fn resolve(
    opts: Arc<Options>,
    crate_name: CrateName,
    curr_version: Option<Version>,
) -> Result<Resolution, BinstallError> {
    let crate_name_name = crate_name.name.clone();
    let resolution = resolve_inner(opts, crate_name, curr_version)
        .await
        .map_err(|err| err.crate_context(crate_name_name))?;

    Ok(resolution)
}

async fn resolve_inner(
    opts: Arc<Options>,
    crate_name: CrateName,
    curr_version: Option<Version>,
) -> Result<Resolution, BinstallError> {
    info!("Resolving package: '{}'", crate_name);

    let version_req = crate_name.version_req.unwrap_or(VersionReq::STAR);

    let version_req_str = version_req.to_compact_string();

    let Some(package_info) = PackageInfo::resolve(
        &opts,
        crate_name.name,
        curr_version,
        &version_req,
        opts.client.clone(),
    )
    .await?
    else {
        return Ok(Resolution::AlreadyUpToDate);
    };

    let desired_targets = opts
        .desired_targets
        .get()
        .await
        .iter()
        .map(|target| {
            debug!("Building metadata for target: {target}");

            let meta = package_info.meta.merge_overrides(
                iter::once(&opts.cli_overrides).chain(package_info.overrides.get(target)),
            );

            debug!("Found metadata: {meta:?}");

            Ok(Arc::new(TargetData {
                target: target.clone(),
                meta,
                target_related_info: TargetTriple::from_str(target)?,
            }))
        })
        .collect::<Result<Vec<_>, BinstallError>>()?;
    let resolvers = &opts.resolvers;

    let binary_name = match package_info.binaries.as_slice() {
        [bin] if bin.name != package_info.name => Some(CompactString::from(bin.name.as_str())),
        _ => None,
    };

    let mut handles: Vec<Arc<dyn Fetcher>> = Vec::with_capacity(
        desired_targets.len() * resolvers.len()
            + if binary_name.is_some() {
                desired_targets.len()
            } else {
                0
            },
    );

    let gh_api_client = opts.gh_api_client.get().await?;

    let mut handles_fn =
        |data: Arc<Data>, filter_fetcher_by_name_predicate: fn(&'static str) -> bool| {
            handles.extend(
                resolvers
                    .iter()
                    .cartesian_product(&desired_targets)
                    .filter_map(|(f, target_data)| {
                        let fetcher = f(
                            opts.client.clone(),
                            gh_api_client.clone(),
                            data.clone(),
                            target_data.clone(),
                            opts.signature_policy,
                        );

                        if let Some(disabled_strategies) =
                            target_data.meta.disabled_strategies.as_deref()
                        {
                            if disabled_strategies.contains(&fetcher.strategy()) {
                                return None;
                            }
                        }

                        filter_fetcher_by_name_predicate(fetcher.fetcher_name()).then_some(fetcher)
                    }),
            )
        };

    handles_fn(
        Arc::new(Data::new(
            package_info.name.clone(),
            package_info.version_str.clone(),
            package_info.repo.clone(),
        )),
        |_| true,
    );

    if let Some(binary_name) = binary_name {
        handles_fn(
            Arc::new(Data::new(
                binary_name,
                package_info.version_str.clone(),
                package_info.repo.clone(),
            )),
            |name| name == FETCHER_GH_CRATE_META,
        );
    }

    for fetcher in &handles {
        match timeout(
            opts.maximum_resolution_timeout,
            AutoAbortJoinHandle::new(fetcher.clone().find()).flattened_join(),
        )
        .await
        {
            Ok(ret) => match ret {
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
                        &opts.bins,
                    )
                    .await
                    {
                        Ok(bin_files) => {
                            if !bin_files.is_empty() {
                                if !opts.disable_telemetry {
                                    fetcher.clone().report_to_upstream();
                                }
                                return Ok(Resolution::Fetch(Box::new(ResolutionFetch {
                                    fetcher: fetcher.clone(),
                                    new_version: package_info.version,
                                    name: package_info.name,
                                    version_req: version_req_str,
                                    source: package_info.source,
                                    bin_files,
                                })));
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
            },
            Err(err) => {
                warn!(
                    "Timeout reached while checking fetcher {}: {}",
                    fetcher.source_name(),
                    err
                );
            }
        }
    }

    // At this point, we don't know whether fallback to cargo install is allowed, or whether it will
    // succeed, but things start to get convoluted when try to include that data, so this will do.
    if !opts.disable_telemetry {
        for fetcher in handles {
            fetcher.report_to_upstream();
        }
    }

    if !opts.cargo_install_fallback {
        return Err(BinstallError::NoFallbackToCargoInstall);
    }

    let meta = package_info
        .meta
        .merge_overrides(iter::once(&opts.cli_overrides));

    let target_meta = desired_targets
        .first()
        .map(|target_data| &target_data.meta)
        .unwrap_or(&meta);

    if let Some(disabled_strategies) = target_meta.disabled_strategies.as_deref() {
        if disabled_strategies.contains(&Strategy::Compile) {
            return Err(BinstallError::NoFallbackToCargoInstall);
        }
    }

    Ok(Resolution::InstallFromSource(ResolutionSource {
        name: package_info.name,
        version: package_info.version_str,
    }))
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
    bins: &Option<Vec<CompactString>>,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    // Download and extract it.
    // If that fails, then ignore this fetcher.
    let extracted_files = fetcher.fetch_and_extract(bin_path).await?;
    debug!("extracted_files = {extracted_files:#?}");

    // Build final metadata
    let meta = fetcher.target_meta();

    // Verify that all non-optional bin_files exist
    let bin_files = collect_bin_files(
        fetcher,
        package_info,
        meta,
        bin_path,
        install_path,
        no_symlinks,
        &extracted_files,
    )?;

    let name = &package_info.name;

    package_info
        .binaries
        .iter()
        .zip(bin_files)
        .filter_map(|(bin, bin_file)| {
            // skip binaries that were not requested by user
            if bins
                .as_ref()
                .is_some_and(|bins| !bins.iter().any(|b| b == bin.name))
            {
                return None;
            }

            match bin_file.check_source_exists(&mut |p| extracted_files.has_file(p)) {
                Ok(()) => Some(Ok(bin_file)),

                // This binary is optional
                Err(err) => {
                    let required_features = &bin.required_features;
                    let bin_name = bin.name.as_str();

                    if required_features.is_empty() {
                        error!(
                            "When resolving {name} bin {bin_name} is not found. \
This binary is not optional so it must be included in the archive, please contact with \
upstream to fix this issue."
                        );
                        // This bin is not optional, error
                        Some(Err(err))
                    } else {
                        // Optional, print a warning and continue.
                        let features = required_features.iter().format(",");
                        warn!(
                            "When resolving {name} bin {bin_name} is not found. \
But since it requires features {features}, this bin is ignored."
                        );
                        None
                    }
                }
            }
        })
        .collect::<Result<Vec<bins::BinFile>, bins::Error>>()
        .map_err(BinstallError::from)
}

fn collect_bin_files(
    fetcher: &dyn Fetcher,
    package_info: &PackageInfo,
    meta: PkgMeta,
    bin_path: &Path,
    install_path: &Path,
    no_symlinks: bool,
    extracted_files: &ExtractedFiles,
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
        target_related_info: &fetcher.target_data().target_related_info,
    };

    let bin_dir = bin_data
        .meta
        .bin_dir
        .as_deref()
        .map(Cow::Borrowed)
        .unwrap_or_else(|| {
            bins::infer_bin_dir_template(&bin_data, &mut |p| extracted_files.get_dir(p).is_some())
        });

    let template = Template::parse(&bin_dir)?;

    // Create bin_files
    let bin_files = package_info
        .binaries
        .iter()
        .map(|bin| bins::BinFile::new(&bin_data, bin.name.as_str(), &template, no_symlinks))
        .collect::<Result<Vec<_>, bins::Error>>()?;

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
    source: CrateSource,
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
        version_req: &VersionReq,
        client: Client,
    ) -> Result<Option<Self>, BinstallError> {
        use CargoTomlFetchOverride::*;

        // Fetch crate via crates.io, git, or use a local manifest path
        let (manifest, source) = match opts.cargo_toml_fetch_override.as_ref() {
            Some(Path(manifest_path)) => (
                spawn_blocking({
                    let manifest_path = manifest_path.clone();
                    let name = name.clone();

                    move || load_manifest_path(manifest_path, &name)
                })
                .await??,
                CrateSource {
                    source_type: SourceType::Path,
                    url: MaybeOwned::Owned(Url::parse(&format!(
                        "file://{}",
                        manifest_path.display()
                    ))?),
                },
            ),
            #[cfg(feature = "git")]
            Some(Git(git_url)) => {
                use crate::helpers::git::{GitCancellationToken, Repository as GitRepository};

                let cancellation_token = GitCancellationToken::default();
                // Cancel git operation if the future is cancelled (dropped).
                let cancel_on_drop = cancellation_token.clone().cancel_on_drop();

                let (ret, commit_hash) = spawn_blocking({
                    let git_url = git_url.clone();
                    let name = name.clone();
                    move || {
                        let dir = tempfile::TempDir::new()?;
                        let repo = GitRepository::shallow_clone(
                            git_url,
                            dir.as_ref(),
                            Some(cancellation_token),
                        )?;

                        Ok::<_, BinstallError>((
                            load_manifest_from_workspace(dir.as_ref(), &name)
                                .map_err(BinstallError::from)?,
                            repo.get_head_commit_hash()?,
                        ))
                    }
                })
                .await??;

                // Git operation done, disarm it
                cancel_on_drop.disarm();

                (
                    ret,
                    CrateSource {
                        source_type: SourceType::Git,
                        url: MaybeOwned::Owned(Url::parse(&format!("{git_url}#{commit_hash}"))?),
                    },
                )
            }
            None => (
                Box::pin(
                    opts.registry
                        .fetch_crate_matched(client, &name, version_req),
                )
                .await?,
                opts.registry.crate_source()?,
            ),
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
                source,
                version_str: new_version_str,
                version: new_version,
                repo: package.repository().map(ToString::to_string),
            }))
        }
    }
}

/// Load binstall metadata from the crate `Cargo.toml` at the provided path
///
/// This is a blocking function.
pub fn load_manifest_path<P: AsRef<Path>, N: AsRef<str>>(
    manifest_path: P,
    name: N,
) -> Result<Manifest<Meta>, BinstallError> {
    fn inner(manifest_path: &Path, crate_name: &str) -> Result<Manifest<Meta>, BinstallError> {
        debug!(
            "Reading crate {crate_name} manifest at local path: {}",
            manifest_path.display()
        );

        // Load and parse manifest (this checks file system for binary output names)
        let manifest = load_manifest_from_workspace(manifest_path, crate_name)?;

        // Return metadata
        Ok(manifest)
    }

    inner(manifest_path.as_ref(), name.as_ref())
}
