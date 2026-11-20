use std::{
    borrow::Cow, collections::BTreeSet, future::Future, iter, mem, path::Path, str::FromStr,
    sync::Arc, time::Duration,
};

use binstalk_fetchers::FETCHER_GH_CRATE_META;
use binstalk_types::{
    cargo_toml_binstall::{BinaryMeta, Strategy},
    crate_info::{CrateSource, SourceType},
};
use compact_str::{CompactString, ToCompactString};
use either::Either;
use itertools::Itertools;
use leon::Template;
use maybe_owned::MaybeOwned;
use semver::{Comparator, Op as ComparatorOp, Version, VersionReq};
use tokio::{
    task::spawn_blocking,
    time::{Instant, error::Elapsed, timeout, timeout_at},
};
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
    manifests::cargo_toml_binstall::{Meta, PkgMeta, PkgOverrides},
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

            let target_related_info = TargetTriple::from_str(target)?;

            let cfgs = target_related_info.cfgs();
            let meta = package_info.meta.merge_overrides(
                iter::once(&opts.cli_overrides)
                    .chain(package_info.overrides.get_matching(target, &cfgs)),
            );

            debug!("Found metadata: {meta:?}");

            Ok(Arc::new(TargetData {
                target: target.clone(),
                meta,
                target_related_info,
            }))
        })
        .collect::<Result<Vec<_>, BinstallError>>()?;
    let resolvers = &opts.resolvers;

    let binary_name = match package_info.binaries.as_slice() {
        [bin] if bin.name != package_info.name => Some(CompactString::from(bin.name.as_str())),
        _ => None,
    };

    // Indices of the binaries the user asked for, used when each binary is
    // downloaded from its own package.
    let selected_bins: Vec<usize> = package_info
        .binaries
        .iter()
        .enumerate()
        .filter(|(_, bin)| {
            opts.bins
                .as_ref()
                .is_none_or(|bins| bins.iter().any(|b| b == bin.name))
        })
        .map(|(i, _)| i)
        .collect();

    let mut candidates: Vec<Candidate> = Vec::with_capacity(
        desired_targets.len() * resolvers.len()
            + if binary_name.is_some() {
                desired_targets.len()
            } else {
                0
            },
    );

    let gh_api_client = opts.gh_api_client.get().await?;

    let mut candidates_fn = |data: Arc<Data>,
                             filter_fetcher_by_name_predicate: fn(&'static str) -> bool,
                             skip_per_bin: bool| {
        for (f, target_data) in resolvers.iter().cartesian_product(&desired_targets) {
            let new_fetcher = |data: Arc<Data>| {
                f(
                    opts.client.clone(),
                    gh_api_client.clone(),
                    data,
                    target_data.clone(),
                    opts.signature_policy,
                )
            };
            let fetcher = new_fetcher(data.clone());

            if let Some(disabled_strategies) = target_data.meta.disabled_strategies.as_deref()
                && disabled_strategies.contains(&fetcher.strategy())
            {
                continue;
            }

            if !filter_fetcher_by_name_predicate(fetcher.fetcher_name()) {
                continue;
            }

            if fetcher.fetcher_name() == FETCHER_GH_CRATE_META && is_pkg_url_per_bin(target_data) {
                if skip_per_bin {
                    continue;
                }
                candidates.push(Candidate::PerBin(
                    selected_bins
                        .iter()
                        .map(|&i| {
                            let bin = package_info.binaries[i].name.as_str();
                            (i, new_fetcher(Arc::new(data.with_bin(bin.into()))))
                        })
                        .collect(),
                ));
            } else {
                candidates.push(Candidate::Package(fetcher));
            }
        }
    };

    candidates_fn(
        Arc::new(Data::new(
            package_info.name.clone(),
            package_info.version_str.clone(),
            package_info.repo.clone(),
        )),
        |_| true,
        false,
    );

    if let Some(binary_name) = binary_name {
        // `{ bin }` already names the binary, so there is no need to try the
        // binary name as `{ name }` for a pkg-url that has it.
        candidates_fn(
            Arc::new(Data::new(
                binary_name,
                package_info.version_str.clone(),
                package_info.repo.clone(),
            )),
            |name| name == FETCHER_GH_CRATE_META,
            true,
        );
    }

    for candidate in &candidates {
        let ret = match candidate {
            Candidate::Package(fetcher) => {
                if !find(fetcher.clone(), opts.maximum_resolution_timeout).await {
                    continue;
                }

                // Generate temporary binary path
                let bin_path = opts.temp_dir.join(format!(
                    "bin-{}-{}-{}",
                    package_info.name,
                    fetcher.target(),
                    fetcher.fetcher_name()
                ));

                download_extract_and_verify(
                    fetcher.as_ref(),
                    &bin_path,
                    &package_info,
                    &package_info.binaries.iter().collect::<Vec<_>>(),
                    false,
                    &opts.install_path,
                    opts.no_symlinks,
                    &opts.bins,
                )
                .await
                .map(|bin_files| Some((fetcher.clone(), bin_files)))
            }
            Candidate::PerBin(fetchers) => {
                download_extract_and_verify_per_bin(fetchers, &package_info, &opts).await
            }
        };

        match ret {
            Ok(Some((fetcher, bin_files))) => {
                if !bin_files.is_empty() {
                    if !opts.disable_telemetry {
                        candidate.report_to_upstream();
                    }
                    return Ok(Resolution::Fetch(Box::new(ResolutionFetch {
                        fetcher,
                        new_version: package_info.version,
                        pubtime: package_info.pubtime,
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
            Ok(None) => (),
            Err(err) => {
                if let BinstallError::UserAbort = err {
                    return Err(err);
                }
                warn!(
                    "Error while downloading and extracting from fetcher {}: {}",
                    candidate.source_name(),
                    err
                );
            }
        }
    }

    // At this point, we don't know whether fallback to cargo install is allowed, or whether it will
    // succeed, but things start to get convoluted when try to include that data, so this will do.
    if !opts.disable_telemetry {
        for candidate in &candidates {
            candidate.report_to_upstream();
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

    if let Some(disabled_strategies) = target_meta.disabled_strategies.as_deref()
        && disabled_strategies.contains(&Strategy::Compile)
    {
        return Err(BinstallError::NoFallbackToCargoInstall);
    }

    fn has_aliases<'a>(mut binary: impl Iterator<Item = &'a BinaryMeta>) -> bool {
        binary.any(|bin_meta| !bin_meta.aliases.is_empty())
    }

    // `cargo install` installs and records the binaries itself, so aliases
    // created next to them would not be removed by `cargo uninstall`.
    if has_aliases(opts.cli_overrides.binary.values()) {
        // The user asked for them explicitly with `--aliases`.
        return Err(BinstallError::CargoInstallDoesNotSupportAliases);
    } else if has_aliases(target_meta.binary.values()) {
        // Set by the crate publisher: erroring would leave targets without a
        // prebuilt binary unable to install the crate at all.
        warn!(
            "Aliases of {} are not created when installing from source",
            package_info.name
        );
    }

    Ok(Resolution::InstallFromSource(ResolutionSource {
        name: package_info.name,
        version: package_info.version_str,
        pubtime: package_info.pubtime,
    }))
}

/// A way to get the binaries of a crate for one target.
enum Candidate {
    /// One package that provides all the binaries.
    Package(Arc<dyn Fetcher>),
    /// One package per binary, from the same fetcher and target, keyed by
    /// the index of the binary in [`PackageInfo::binaries`].
    ///
    /// Used when `pkg-url` contains key `bin`.
    PerBin(Vec<(usize, Arc<dyn Fetcher>)>),
}

impl Candidate {
    fn fetchers(&self) -> impl Iterator<Item = &Arc<dyn Fetcher>> {
        match self {
            Candidate::Package(fetcher) => Either::Left(iter::once(fetcher)),
            Candidate::PerBin(fetchers) => Either::Right(fetchers.iter().map(|(_, f)| f)),
        }
    }

    fn report_to_upstream(&self) {
        for fetcher in self.fetchers() {
            fetcher.clone().report_to_upstream();
        }
    }

    fn source_name(&self) -> CompactString {
        self.fetchers()
            .map(|fetcher| fetcher.source_name())
            .unique()
            .join(", ")
            .into()
    }
}

/// Whether each binary is downloaded from its own package, which is the case
/// when `pkg-url` contains key `bin`.
fn is_pkg_url_per_bin<T: leon::Values + ?Sized>(target_data: &TargetData<T>) -> bool {
    target_data
        .meta
        .pkg_url
        .as_deref()
        .and_then(|pkg_url| Template::parse(pkg_url).ok())
        .is_some_and(|template| template.has_key("bin"))
}

/// Returns whether `fetcher.find()` found a package, logging errors and
/// timeouts.
async fn log_find_result(
    fetcher: &dyn Fetcher,
    ret: impl Future<Output = Result<Result<bool, BinstallError>, Elapsed>>,
) -> bool {
    match ret.await {
        Ok(Ok(found)) => found,
        Ok(Err(err)) => {
            warn!(
                "Error while checking fetcher {}: {}",
                fetcher.source_name(),
                err
            );
            false
        }
        Err(err) => {
            warn!(
                "Timeout reached while checking fetcher {}: {}",
                fetcher.source_name(),
                err
            );
            false
        }
    }
}

async fn find(fetcher: Arc<dyn Fetcher>, maximum_resolution_timeout: Duration) -> bool {
    log_find_result(
        fetcher.as_ref(),
        timeout(
            maximum_resolution_timeout,
            AutoAbortJoinHandle::new(fetcher.clone().find()).flattened_join(),
        ),
    )
    .await
}

/// Find, download and extract the package of each binary in `fetchers`.
///
/// Returns `None` if the package of a binary that is not optional cannot be
/// found, or if no package is found at all.
///
/// Otherwise, returns one of the fetchers, for its target and source name,
/// and the binaries found.
async fn download_extract_and_verify_per_bin(
    fetchers: &[(usize, Arc<dyn Fetcher>)],
    package_info: &PackageInfo,
    opts: &Options,
) -> Result<Option<(Arc<dyn Fetcher>, Vec<bins::BinFile>)>, BinstallError> {
    let name = &package_info.name;
    let deadline = Instant::now() + opts.maximum_resolution_timeout;

    // Start all of them first, so that they run concurrently.
    let handles = fetchers
        .iter()
        .map(|(i, fetcher)| {
            (
                &package_info.binaries[*i],
                fetcher,
                AutoAbortJoinHandle::new(fetcher.clone().find()),
            )
        })
        .collect::<Vec<_>>();

    let mut found = Vec::with_capacity(handles.len());
    for (bin, fetcher, handle) in handles {
        let bin_name = bin.name.as_str();

        if log_find_result(
            fetcher.as_ref(),
            timeout_at(deadline, handle.flattened_join()),
        )
        .await
        {
            found.push((bin, fetcher));
        } else if bin.required_features.is_empty() {
            debug!(
                "The package of {name} bin {bin_name} is not found on target {}",
                fetcher.target()
            );
            return Ok(None);
        } else {
            let features = bin.required_features.iter().format(",");
            warn!(
                "The package of {name} bin {bin_name} is not found. \
But since it requires features {features}, this bin is ignored."
            );
        }
    }

    let Some((_, first_fetcher)) = found.first() else {
        return Ok(None);
    };
    let first_fetcher = Arc::clone(first_fetcher);

    let mut bin_files = Vec::with_capacity(found.len());
    for (bin, fetcher) in found {
        // Generate temporary binary path
        let bin_path = opts.temp_dir.join(format!(
            "bin-{}-{}-{}-{}",
            package_info.name,
            fetcher.target(),
            fetcher.fetcher_name(),
            bin.name,
        ));

        bin_files.extend(
            download_extract_and_verify(
                fetcher.as_ref(),
                &bin_path,
                package_info,
                &[bin],
                true,
                &opts.install_path,
                opts.no_symlinks,
                &opts.bins,
            )
            .await?,
        );
    }

    bins::check_aliases_unique(&bin_files)?;

    Ok(Some((first_fetcher, bin_files)))
}

///  * `fetcher` - `fetcher.find()` must have returned `Ok(true)`.
///
///  * `binaries` - the binaries the package provides.
///  * `per_bin` - whether the package only provides the binary in `binaries`,
///    i.e. pkg-url contains key `bin`.
///
/// Can return empty Vec if all `BinFile` is optional and does not exist
/// in the archive downloaded.
#[allow(clippy::too_many_arguments)]
async fn download_extract_and_verify(
    fetcher: &dyn Fetcher,
    bin_path: &Path,
    package_info: &PackageInfo,
    binaries: &[&Bin],
    per_bin: bool,
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
        binaries,
        per_bin,
        meta,
        bin_path,
        install_path,
        no_symlinks,
        &extracted_files,
    )?;

    let name = &package_info.name;

    binaries
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

#[allow(clippy::too_many_arguments)]
fn collect_bin_files(
    fetcher: &dyn Fetcher,
    package_info: &PackageInfo,
    binaries: &[&Bin],
    per_bin: bool,
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
            let mut has_dir = |p: &Path| extracted_files.get_dir(p).is_some();

            // The package of one binary is likely to be named after it,
            // e.g. `{ bin }-{ target }`, so try that first.
            if let (true, [bin]) = (per_bin, binaries) {
                let template = bins::infer_bin_dir_template(
                    &bins::Data {
                        name: &bin.name,
                        meta: PkgMeta::default(),
                        ..bin_data
                    },
                    &mut has_dir,
                );
                if template != bins::DEFAULT_BIN_DIR_TEMPLATE {
                    return template;
                }
            }

            bins::infer_bin_dir_template(&bin_data, &mut has_dir)
        });

    let template = Template::parse(&bin_dir)?;

    if let Some(name) = bin_data.meta.binary.keys().find(|name| {
        !package_info
            .binaries
            .iter()
            .any(|bin| bin.name == name.as_str())
    }) {
        return Err(bins::Error::UnknownBinary(name.clone()).into());
    }

    // Create bin_files
    let bin_files = binaries
        .iter()
        .map(|bin| {
            let aliases = bin_data
                .meta
                .binary
                .get(bin.name.as_str())
                .map(|bin_meta| &*bin_meta.aliases)
                .unwrap_or_default();
            bins::BinFile::new(
                &bin_data,
                bin.name.as_str(),
                &template,
                no_symlinks,
                aliases,
            )
        })
        .collect::<Result<Vec<_>, bins::Error>>()?;

    bins::check_aliases_unique(&bin_files)?;

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
    /// Publish time (RFC 3339) from the registry index, if known
    pubtime: Option<CompactString>,
    repo: Option<String>,
    overrides: PkgOverrides,
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
        let (manifest, source, pubtime) = match opts.cargo_toml_fetch_override.as_ref() {
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
                None,
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
                    None,
                )
            }
            None => {
                let (manifest, pubtime) = Box::pin(opts.registry.fetch_crate_matched(
                    client,
                    &name,
                    version_req,
                ))
                .await?;

                (manifest, opts.registry.crate_source()?, pubtime)
            }
        };

        let Some(mut package) = manifest.package else {
            return Err(BinstallError::CargoTomlMissingPackage(name));
        };

        let manifest_version_str = package.version().to_compact_string();
        let (new_version_str, new_version) = if opts.cargo_toml_fetch_override.is_some() {
            if let Some(version) = exact_version_req(version_req) {
                (version.to_compact_string(), version)
            } else {
                let (version_str, version) = parse_package_version(manifest_version_str)?;
                // `--git` and `--manifest-path` give one manifest, so there is
                // no other version to pick.
                if !version_req.matches(&version) {
                    warn!(
                        "{name}: ignoring version requirement {version_req}, \
                         the manifest found has version {version}"
                    );
                }
                (version_str, version)
            }
        } else {
            parse_package_version(manifest_version_str)?
        };

        if let Some(curr_version) = curr_version
            && new_version == curr_version
        {
            info!(
                "{} v{curr_version} is already installed, use --force to override",
                name
            );
            return Ok(None);
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

        // Bin names come from the crate publisher and are later joined onto
        // the install path, so reject anything that is not a plain file name
        // before downloading anything.
        if let Some(bin) = binaries
            .iter()
            .find(|bin| !bins::is_valid_bin_name(&bin.name))
        {
            return Err(bins::Error::InvalidBinName(bin.name.as_str().into()).into());
        }

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
                pubtime,
                repo: package.repository().map(ToString::to_string),
            }))
        }
    }
}

fn parse_package_version(
    version_str: CompactString,
) -> Result<(CompactString, Version), BinstallError> {
    let version = match Version::parse(&version_str) {
        Ok(version) => version,
        Err(err) => {
            return Err(Box::new(VersionParseError {
                v: version_str,
                err,
            })
            .into());
        }
    };

    Ok((version_str, version))
}

fn exact_version_req(version_req: &VersionReq) -> Option<Version> {
    match version_req.comparators.as_slice() {
        [
            Comparator {
                op: ComparatorOp::Exact,
                major,
                minor: Some(minor),
                patch: Some(patch),
                pre,
            },
        ] => Some(Version {
            major: *major,
            minor: *minor,
            patch: *patch,
            pre: pre.clone(),
            build: Default::default(),
        }),
        _ => None,
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
