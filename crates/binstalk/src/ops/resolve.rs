use std::{borrow::Cow, collections::BTreeSet, iter, mem, path::Path, str::FromStr, sync::Arc};

use binstalk_fetchers::FETCHER_GH_CRATE_META;
use binstalk_types::{
    cargo_toml_binstall::{BinaryMeta, Strategy},
    crate_info::{CrateSource, PkgUrlHint, SourceType},
};
use compact_str::{CompactString, ToCompactString};
use itertools::Itertools;
use leon::Template;
use maybe_owned::MaybeOwned;
use semver::{Comparator, Op as ComparatorOp, Version, VersionReq};
use tokio::{task::spawn_blocking, time::timeout};
use tracing::{debug, error, info, instrument, warn};
use url::Url;

use crate::{
    bins,
    errors::{BinstallError, VersionParseError},
    fetchers::{BinInfo, Data, Fetcher, Found, TargetArchOverride, TargetData},
    helpers::{
        cargo_toml::Manifest, cargo_toml_workspace::load_manifest_from_workspace,
        download::ExtractedFiles, remote::Client, target_triple::TargetTriple,
    },
    manifests::cargo_toml_binstall::{DistMeta, Meta, PkgMeta, PkgOverrides},
    ops::{CargoTomlFetchOverride, Options},
    registry::merge_workspace_dist_meta,
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

/// * `pkg_url_hint` - the target and [`PkgUrlHint`] the installed version was
///   found with, if any, see [`Data::with_pkg_url_hint`].
#[instrument(skip_all)]
pub async fn resolve(
    opts: Arc<Options>,
    crate_name: CrateName,
    curr_version: Option<Version>,
    pkg_url_hint: Option<(CompactString, PkgUrlHint)>,
) -> Result<Resolution, BinstallError> {
    let crate_name_name = crate_name.name.clone();
    let resolution = resolve_inner(opts, crate_name, curr_version, pkg_url_hint)
        .await
        .map_err(|err| err.crate_context(crate_name_name))?;

    Ok(resolution)
}

async fn resolve_inner(
    opts: Arc<Options>,
    crate_name: CrateName,
    curr_version: Option<Version>,
    pkg_url_hint: Option<(CompactString, PkgUrlHint)>,
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

    let mut handles: Vec<Arc<dyn Fetcher>> = Vec::with_capacity(
        desired_targets.len() * resolvers.len()
            + if binary_name.is_some() {
                desired_targets.len()
            } else {
                0
            },
    );

    let gh_api_client = opts.gh_api_client.get().await?;

    // The binaries the user asked for, for a `pkg-url` with one package per
    // binary.
    let bin_infos: Vec<BinInfo> = package_info
        .binaries
        .iter()
        .filter(|bin| is_requested(&opts.bins, bin))
        .map(|bin| BinInfo {
            name: bin.name.as_str().into(),
            optional: !bin.required_features.is_empty(),
        })
        .collect();

    let with_pkg_url_hint = |data: Data| {
        let data = data
            .with_bins(bin_infos.clone())
            .with_dist_meta(package_info.dist.clone());
        match pkg_url_hint.clone() {
            Some((target, hint)) => data.with_pkg_url_hint(target, hint),
            None => data,
        }
    };

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
                            !opts.disable_telemetry,
                        );

                        if let Some(disabled_strategies) =
                            target_data.meta.disabled_strategies.as_deref()
                            && disabled_strategies.contains(&fetcher.strategy())
                        {
                            return None;
                        }

                        filter_fetcher_by_name_predicate(fetcher.fetcher_name()).then_some(fetcher)
                    }),
            )
        };

    let data = Data::new(
        opts.fetch_context.clone(),
        package_info.name.clone(),
        package_info.version_str.clone(),
        package_info.repo.clone(),
    );
    // Shares what is known about the repository with `data`.
    let binary_data = binary_name.map(|binary_name| data.with_name(binary_name));

    handles_fn(Arc::new(with_pkg_url_hint(data)), |_| true);

    if let Some(binary_data) = binary_data {
        handles_fn(Arc::new(with_pkg_url_hint(binary_data)), |name| {
            name == FETCHER_GH_CRATE_META
        });
    }

    for fetcher in &handles {
        let mut found = match timeout(opts.maximum_resolution_timeout, fetcher.clone().find()).await
        {
            Ok(Ok(Some(found))) => found,
            Ok(Ok(None)) => continue,
            Ok(Err(err)) => {
                warn!(
                    "Error while checking fetcher {}: {}",
                    fetcher.fetcher_name(),
                    err
                );
                continue;
            }
            Err(err) => {
                warn!(
                    "Timeout reached while checking fetcher {}: {}",
                    fetcher.fetcher_name(),
                    err
                );
                continue;
            }
        };

        // Generate temporary binary path
        let bin_path = opts.temp_dir.join(format!(
            "bin-{}-{}-{}",
            package_info.name,
            found.target(),
            fetcher.fetcher_name()
        ));

        let ret = download_extract_and_verify(
            found.as_mut(),
            &bin_path,
            &package_info,
            &opts.install_path,
            opts.no_symlinks,
            &opts.bins,
        )
        .await;

        match ret {
            Ok(bin_files) if !bin_files.is_empty() => {
                return Ok(Resolution::Fetch(Box::new(ResolutionFetch {
                    found,
                    new_version: package_info.version,
                    pubtime: package_info.pubtime,
                    name: package_info.name,
                    version_req: version_req_str,
                    source: package_info.source,
                    bin_files,
                })));
            }
            Ok(_) => warn!(
                "Error when checking binaries provided by fetcher {}: \
                The fetcher does not provide any optional binary",
                found.source_name(),
            ),
            Err(BinstallError::UserAbort) => return Err(BinstallError::UserAbort),
            Err(err) => warn!(
                "Error while downloading and extracting from fetcher {}: {}",
                found.source_name(),
                err
            ),
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

///  * `found` - What a fetcher's `find()` returned.
///
/// Can return empty Vec if all `BinFile` is optional and does not exist
/// in the archive downloaded.
async fn download_extract_and_verify(
    found: &mut dyn Found,
    bin_path: &Path,
    package_info: &PackageInfo,
    install_path: &Path,
    no_symlinks: bool,
    bins: &Option<Vec<CompactString>>,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    // Download and extract it, unless another crate did already: then it is
    // somewhere else than `bin_path`.
    // If that fails, then ignore this fetcher.
    let packages = found.fetch_and_extract(bin_path).await?;
    debug!("packages = {packages:#?}");

    let mut bin_files = Vec::with_capacity(package_info.binaries.len());

    for package in packages {
        // The binaries in this package: all of them, or its own one.
        let binaries = package_info
            .binaries
            .iter()
            .filter(|bin| package.bin.as_deref().is_none_or(|name| name == bin.name))
            .collect::<Vec<_>>();
        let extracted = &package.extracted;

        let files = collect_bin_files(
            found,
            package_info,
            &binaries,
            package.meta,
            extracted.dir(),
            install_path,
            no_symlinks,
            extracted.files(),
        )?;

        bin_files.extend(verify_bin_files(
            package_info,
            &binaries,
            files,
            extracted.files(),
            bins,
        )?);
    }

    bins::check_aliases_unique(&bin_files)?;

    Ok(bin_files)
}

/// Whether the user asked for `bin`, with `--bin`.
fn is_requested(bins: &Option<Vec<CompactString>>, bin: &Bin) -> bool {
    bins.as_ref()
        .is_none_or(|bins| bins.iter().any(|b| b == bin.name))
}

/// Verify that all non-optional `bin_files`, made for `binaries`, exist.
fn verify_bin_files(
    package_info: &PackageInfo,
    binaries: &[&Bin],
    bin_files: Vec<bins::BinFile>,
    extracted_files: &ExtractedFiles,
    bins: &Option<Vec<CompactString>>,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    let name = &package_info.name;

    binaries
        .iter()
        .zip(bin_files)
        .filter_map(|(bin, bin_file)| {
            // skip binaries that were not requested by user
            if !is_requested(bins, bin) {
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
    found: &dyn Found,
    package_info: &PackageInfo,
    binaries: &[&Bin],
    meta: PkgMeta,
    bin_path: &Path,
    install_path: &Path,
    no_symlinks: bool,
    extracted_files: &ExtractedFiles,
) -> Result<Vec<bins::BinFile>, BinstallError> {
    // `bin-dir` must use the same `{ target-arch }` as the URL the package
    // was found at.
    let target_related_info = TargetArchOverride {
        values: &found.target_data().target_related_info,
        target_arch: found.resolved_target_arch(),
    };

    // List files to be installed
    // based on those found via Cargo.toml
    let bin_data = bins::Data {
        name: &package_info.name,
        target: found.target(),
        version: &package_info.version_str,
        repo: package_info.repo.as_deref(),
        meta,
        bin_path,
        install_path,
        target_related_info: &target_related_info,
    };

    let bin_dir = bin_data
        .meta
        .bin_dir
        .as_deref()
        .map(Cow::Borrowed)
        .unwrap_or_else(|| {
            let sole_root_dir = extracted_files
                .get_dir(Path::new("."))
                .and_then(|names| names.iter().exactly_one().ok())
                .filter(|name| extracted_files.get_dir(Path::new(name)).is_some())
                .and_then(|name| name.to_str());
            bins::infer_bin_dir_template(
                &bin_data,
                &mut |p| extracted_files.get_dir(p).is_some(),
                sole_root_dir,
            )
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
                &mut |p| extracted_files.get_dir(p).is_some(),
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
    /// The crate's dist config, see [`Meta::dist`].
    dist: Option<DistMeta>,
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
            Some(Git(git_url, reference)) => {
                use crate::helpers::git::{GitCancellationToken, Repository as GitRepository};

                let cancellation_token = GitCancellationToken::default();
                // Cancel git operation if the future is cancelled (dropped).
                let cancel_on_drop = cancellation_token.clone().cancel_on_drop();

                let (ret, commit_hash) = spawn_blocking({
                    let git_url = git_url.clone();
                    let reference = reference.clone();
                    let name = name.clone();
                    move || {
                        let dir = tempfile::TempDir::new()?;
                        let repo = GitRepository::shallow_clone_reference(
                            git_url,
                            dir.as_ref(),
                            reference.as_borrowed(),
                            Some(cancellation_token),
                        )?;

                        let mut manifest = load_manifest_from_workspace(dir.as_ref(), &name)
                            .map_err(BinstallError::from)?;
                        merge_workspace_dist_meta(dir.as_ref(), &mut manifest);
                        Ok::<_, BinstallError>((manifest, repo.get_head_commit_hash()?))
                    }
                })
                .await??;

                // Git operation done, disarm it
                cancel_on_drop.disarm();

                (
                    ret,
                    CrateSource {
                        source_type: SourceType::Git,
                        url: MaybeOwned::Owned(git_source_url(
                            git_url,
                            reference.as_borrowed(),
                            commit_hash,
                        )?),
                    },
                    None,
                )
            }
            None => {
                let matched =
                    Box::pin(opts.registry.find_crate_matched(client, &name, version_req)).await?;

                // The registry index already says which version would be
                // installed, so don't download the crate just to find out
                // it is up to date.
                if let Some(curr_version) = &curr_version
                    && Version::parse(matched.version()).is_ok_and(|v| v == *curr_version)
                {
                    info!(
                        "{} v{curr_version} is already installed, use --force to override",
                        name
                    );
                    return Ok(None);
                }

                let pubtime = matched.pubtime().map(CompactString::from);
                let manifest = Box::pin(matched.fetch_manifest()).await?;

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

        let metadata = package.metadata.take();
        let dist = metadata.as_ref().and_then(|m| m.dist.clone());
        if let Some(dist) = &dist {
            debug!("Found dist config: {dist:?}");
        }
        let (mut meta, binaries): (_, Vec<Bin>) = (
            metadata.and_then(|m| m.binstall).unwrap_or_default(),
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
                dist,
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

/// The source url of a crate installed from git, in the format cargo writes
/// to `.crates.toml`: `{url}?branch={branch}#{commit}` (or `tag=`/`rev=`,
/// nothing for the default branch).
#[cfg(feature = "git")]
fn git_source_url(
    git_url: &crate::helpers::git::GitUrl,
    reference: crate::helpers::git::GitReference<'_>,
    commit_hash: impl std::fmt::Display,
) -> Result<Url, BinstallError> {
    use crate::helpers::git::GitReference::*;

    let mut url = Url::parse(&git_url.to_string())?;
    let query = match reference {
        Branch(branch) => Some(("branch", branch)),
        Tag(tag) => Some(("tag", tag)),
        Rev(rev) => Some(("rev", rev)),
        _ => None,
    };
    if let Some((key, value)) = query {
        url.query_pairs_mut().append_pair(key, value);
    }
    url.set_fragment(Some(&commit_hash.to_string()));
    Ok(url)
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
        let mut manifest = load_manifest_from_workspace(manifest_path, crate_name)?;
        // The workspace root, if `manifest_path` is that of a workspace.
        let root = if manifest_path.is_file() {
            manifest_path.parent().unwrap_or(Path::new("."))
        } else {
            manifest_path
        };
        merge_workspace_dist_meta(root, &mut manifest);

        // Return metadata
        Ok(manifest)
    }

    inner(manifest_path.as_ref(), name.as_ref())
}

#[cfg(all(test, feature = "git"))]
mod tests {
    use super::*;
    use crate::helpers::git::{GitReference, GitUrl};

    #[test]
    fn git_source_url_matches_cargo() {
        let url: GitUrl = "https://github.com/cargo-bins/cargo-binstall"
            .parse()
            .unwrap();
        let commit = "0123456789abcdef0123456789abcdef01234567";

        for (reference, expected) in [
            (GitReference::DefaultBranch, ""),
            (GitReference::Branch("main"), "?branch=main"),
            (GitReference::Tag("v1.0.0"), "?tag=v1.0.0"),
            (GitReference::Rev("abc123"), "?rev=abc123"),
            (GitReference::Branch("a b/c&d"), "?branch=a+b%2Fc%26d"),
        ] {
            assert_eq!(
                git_source_url(&url, reference, commit).unwrap().as_str(),
                format!("https://github.com/cargo-bins/cargo-binstall{expected}#{commit}"),
                "{reference:?}"
            );
        }
    }
}
