use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use binstalk::{
    errors::{BinstallError, CrateContextError},
    fetchers::{Fetcher, GhCrateMeta, QuickInstall, SignaturePolicy},
    get_desired_targets,
    helpers::{
        jobserver_client::LazyJobserverClient,
        lazy_gh_api_client::LazyGhApiClient,
        remote::{Certificate, Client},
        tasks::AutoAbortJoinHandle,
    },
    ops::{
        self,
        resolve::{CrateName, Resolution, ResolutionFetch, VersionReqExt},
        CargoTomlFetchOverride, Options, Resolver,
    },
};
use binstalk_manifests::{
    cargo_config::Config,
    cargo_toml_binstall::{PkgOverride, Strategy},
    crates_manifests::Manifests,
};
use file_format::FileFormat;
use home::cargo_home;
use log::LevelFilter;
use miette::{miette, Report, Result, WrapErr};
use tokio::task::block_in_place;
use tracing::{debug, error, info, warn};

use crate::{args::Args, gh_token, git_credentials, install_path, ui::confirm};

pub fn install_crates(
    args: Args,
    jobserver_client: LazyJobserverClient,
) -> Result<Option<AutoAbortJoinHandle<Result<()>>>> {
    // Compute Resolvers
    let mut cargo_install_fallback = false;

    let resolvers: Vec<_> = args
        .strategies
        .into_iter()
        .filter_map(|strategy| match strategy.0 {
            Strategy::CrateMetaData => Some(GhCrateMeta::new as Resolver),
            Strategy::QuickInstall => Some(QuickInstall::new as Resolver),
            Strategy::Compile => {
                cargo_install_fallback = true;
                None
            }
        })
        .collect();

    // Load .cargo/config.toml
    let cargo_home = cargo_home().map_err(BinstallError::from)?;
    let mut config = Config::load_from_path(cargo_home.join("config.toml"))?;

    // Compute paths
    let cargo_root = args.root;
    let (install_path, mut manifests, temp_dir) = compute_paths_and_load_manifests(
        cargo_root.clone(),
        args.install_path,
        args.no_track,
        cargo_home,
        &mut config,
    )?;

    // Remove installed crates
    let mut crate_names =
        filter_out_installed_crates(args.crate_names, args.force, manifests.as_mut())?.peekable();

    if crate_names.peek().is_none() {
        debug!("Nothing to do");
        return Ok(None);
    }

    // Launch target detection
    let desired_targets = get_desired_targets(args.targets);

    // Computer cli_overrides
    let cli_overrides = PkgOverride {
        pkg_url: args.pkg_url    
        pkg_fmt: args.pkg_fmt,
        bin_dir: args.bin_dir,
        disabled_strategies: (!args.disabled_strategies.is_empty()).then_some(|| {
            args.disabled_strategies
                .into_iter()
                .map(|strategy| strategy.0)
                .collect()
        }),
        signing: None,
    };

    // Initialize reqwest client
    let rate_limit = args.rate_limit;

    let mut http = config.http.take();

    let client = Client::new(
        concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
        args.min_tls_version.map(|v| v.into()),
        rate_limit.duration,
        rate_limit.request_count,
        read_root_certs(
            args.root_certificates,
            http.as_mut().and_then(|http| http.cainfo.take()),
        ),
    )
    .map_err(BinstallError::from)?;

    let gh_api_client = args
        .github_token
        .map(|token| token.0)
        .or_else(|| {
            if args.no_discover_github_token {
                None
            } else {
                git_credentials::try_from_home()
            }
        })
        .map(|token| LazyGhApiClient::new(client.clone(), Some(token)))
        .unwrap_or_else(|| {
            if args.no_discover_github_token {
                LazyGhApiClient::new(client.clone(), None)
            } else {
                LazyGhApiClient::with_get_gh_token_future(client.clone(), async {
                    match gh_token::get().await {
                        Ok(token) => Some(token),
                        Err(err) => {
                            debug!(?err, "Failed to retrieve token from `gh auth token`");
                            debug!("Failed to read git credential file");
                            None
                        }
                    }
                })
            }
        });

    // Create binstall_opts
    let binstall_opts = Arc::new(Options {
        no_symlinks: args.no_symlinks,
        dry_run: args.dry_run,
        force: args.force,
        quiet: args.log_level == Some(LevelFilter::Off),
        locked: args.locked,
        no_track: args.no_track,

        version_req: args.version_req,
        #[cfg(feature = "git")]
        cargo_toml_fetch_override: match (args.manifest_path, args.git) {
            (Some(manifest_path), None) => Some(CargoTomlFetchOverride::Path(manifest_path)),
            (None, Some(git_url)) => Some(CargoTomlFetchOverride::Git(git_url)),
            (None, None) => None,
            _ => unreachable!("manifest_path and git cannot be specified at the same time"),
        },

        #[cfg(not(feature = "git"))]
        cargo_toml_fetch_override: args.manifest_path.map(CargoTomlFetchOverride::Path),
        cli_overrides,

        desired_targets,
        resolvers,
        cargo_install_fallback,

        temp_dir: temp_dir.path().to_owned(),
        install_path,
        cargo_root,

        client,
        gh_api_client,
        jobserver_client,
        registry: if let Some(index) = args.index {
            index
        } else if let Some(registry_name) = args
            .registry
            .or_else(|| config.registry.and_then(|registry| registry.default))
        {
            let registry_name_lowercase = registry_name.to_lowercase();

            let v = env::vars().find_map(|(k, v)| {
                let name_lowercase = k
                    .strip_prefix("CARGO_REGISTRIES_")?
                    .strip_suffix("_INDEX")?
                    .to_lowercase();

                (name_lowercase == registry_name_lowercase).then_some(v)
            });

            if let Some(v) = &v {
                v
            } else {
                config
                    .registries
                    .as_ref()
                    .and_then(|registries| registries.get(&registry_name))
                    .and_then(|registry| registry.index.as_deref())
                    .ok_or_else(|| BinstallError::UnknownRegistryName(registry_name))?
            }
            .parse()
            .map_err(BinstallError::from)?
        } else {
            Default::default()
        },

        signature_policy: if args.only_signed {
            SignaturePolicy::Require
        } else if args.skip_signatures {
            SignaturePolicy::Ignore
        } else {
            SignaturePolicy::IfPresent
        },
        disable_telemetry: args.disable_telemetry,
    });

    // Destruct args before any async function to reduce size of the future
    let dry_run = args.dry_run;
    let no_confirm = args.no_confirm;
    let no_cleanup = args.no_cleanup;

    // Resolve crates
    let tasks: Vec<_> = crate_names
        .map(|(crate_name, current_version)| {
            AutoAbortJoinHandle::spawn(ops::resolve::resolve(
                binstall_opts.clone(),
                crate_name,
                current_version,
            ))
        })
        .collect();

    Ok(Some(if args.continue_on_failure {
        AutoAbortJoinHandle::spawn(async move {
            // Collect results
            let mut resolution_fetchs = Vec::new();
            let mut resolution_sources = Vec::new();
            let mut errors = Vec::new();

            for task in tasks {
                match task.flattened_join().await {
                    Ok(Resolution::AlreadyUpToDate) => {}
                    Ok(Resolution::Fetch(fetch)) => {
                        fetch.print(&binstall_opts);
                        resolution_fetchs.push(fetch)
                    }
                    Ok(Resolution::InstallFromSource(source)) => {
                        source.print();
                        resolution_sources.push(source)
                    }
                    Err(BinstallError::CrateContext(err)) => errors.push(err),
                    Err(e) => panic!("Expected BinstallError::CrateContext(_), got {}", e),
                }
            }

            if resolution_fetchs.is_empty() && resolution_sources.is_empty() {
                return if let Some(err) = BinstallError::crate_errors(errors) {
                    Err(err.into())
                } else {
                    debug!("Nothing to do");
                    Ok(())
                };
            }

            // Confirm
            if !dry_run && !no_confirm {
                if let Err(abort_err) = confirm().await {
                    return if let Some(err) = BinstallError::crate_errors(errors) {
                        Err(Report::new(abort_err).wrap_err(err))
                    } else {
                        Err(abort_err.into())
                    };
                }
            }

            let manifest_update_res = do_install_fetches_continue_on_failure(
                resolution_fetchs,
                manifests,
                &binstall_opts,
                dry_run,
                temp_dir,
                no_cleanup,
                &mut errors,
            );

            let tasks: Vec<_> = resolution_sources
                .into_iter()
                .map(|source| AutoAbortJoinHandle::spawn(source.install(binstall_opts.clone())))
                .collect();

            for task in tasks {
                match task.flattened_join().await {
                    Ok(_) => (),
                    Err(BinstallError::CrateContext(err)) => errors.push(err),
                    Err(e) => panic!("Expected BinstallError::CrateContext(_), got {}", e),
                }
            }

            match (BinstallError::crate_errors(errors), manifest_update_res) {
                (None, Ok(())) => Ok(()),
                (None, Err(err)) => Err(err),
                (Some(err), Ok(())) => Err(err.into()),
                (Some(err), Err(manifest_update_err)) => {
                    Err(Report::new(err).wrap_err(manifest_update_err))
                }
            }
        })
    } else {
        AutoAbortJoinHandle::spawn(async move {
            // Collect results
            let mut resolution_fetchs = Vec::new();
            let mut resolution_sources = Vec::new();

            for task in tasks {
                match task.await?? {
                    Resolution::AlreadyUpToDate => {}
                    Resolution::Fetch(fetch) => {
                        fetch.print(&binstall_opts);
                        resolution_fetchs.push(fetch)
                    }
                    Resolution::InstallFromSource(source) => {
                        source.print();
                        resolution_sources.push(source)
                    }
                }
            }

            if resolution_fetchs.is_empty() && resolution_sources.is_empty() {
                debug!("Nothing to do");
                return Ok(());
            }

            // Confirm
            if !dry_run && !no_confirm {
                confirm().await?;
            }

            do_install_fetches(
                resolution_fetchs,
                manifests,
                &binstall_opts,
                dry_run,
                temp_dir,
                no_cleanup,
            )?;

            let tasks: Vec<_> = resolution_sources
                .into_iter()
                .map(|source| AutoAbortJoinHandle::spawn(source.install(binstall_opts.clone())))
                .collect();

            for task in tasks {
                task.await??;
            }

            Ok(())
        })
    }))
}

fn do_read_root_cert(path: &Path) -> Result<Option<Certificate>, BinstallError> {
    use std::io::{Read, Seek};

    let mut file = fs::File::open(path)?;
    let file_format = FileFormat::from_reader(&mut file)?;

    let open_cert = match file_format {
        FileFormat::PemCertificate => Certificate::from_pem,
        FileFormat::DerCertificate => Certificate::from_der,
        _ => {
            warn!(
                "Unable to load {}: Expected pem or der ceritificate but found {file_format}",
                path.display()
            );

            return Ok(None);
        }
    };

    // Move file back to its head
    file.rewind()?;

    let mut buffer = Vec::with_capacity(200);
    file.read_to_end(&mut buffer)?;

    open_cert(&buffer).map_err(From::from).map(Some)
}

fn read_root_certs(
    root_certificate_paths: Vec<PathBuf>,
    config_cainfo: Option<PathBuf>,
) -> impl Iterator<Item = Certificate> {
    root_certificate_paths
        .into_iter()
        .chain(config_cainfo)
        .filter_map(|path| match do_read_root_cert(&path) {
            Ok(optional_cert) => optional_cert,
            Err(err) => {
                warn!(
                    "Failed to load root certificate at {}: {err}",
                    path.display()
                );
                None
            }
        })
}

/// Return (install_path, manifests, temp_dir)
fn compute_paths_and_load_manifests(
    roots: Option<PathBuf>,
    install_path: Option<PathBuf>,
    no_track: bool,
    cargo_home: PathBuf,
    config: &mut Config,
) -> Result<(PathBuf, Option<Manifests>, tempfile::TempDir)> {
    // Compute cargo_roots
    let cargo_roots =
        install_path::get_cargo_roots_path(roots, cargo_home, config).ok_or_else(|| {
            error!("No viable cargo roots path found of specified, try `--roots`");
            miette!("No cargo roots path found or specified")
        })?;

    // Compute install directory
    let (install_path, custom_install_path) =
        install_path::get_install_path(install_path, Some(&cargo_roots));
    let install_path = install_path.ok_or_else(|| {
        error!("No viable install path found of specified, try `--install-path`");
        miette!("No install path found or specified")
    })?;
    fs::create_dir_all(&install_path).map_err(BinstallError::Io)?;
    debug!("Using install path: {}", install_path.display());

    let no_manifests = no_track || custom_install_path;

    // Load manifests
    let manifests = if !no_manifests {
        Some(Manifests::open_exclusive(&cargo_roots)?)
    } else {
        None
    };

    // Create a temporary directory for downloads etc.
    //
    // Put all binaries to a temporary directory under `dst` first, catching
    // some failure modes (e.g., out of space) before touching the existing
    // binaries. This directory will get cleaned up via RAII.
    let temp_dir = tempfile::Builder::new()
        .prefix("cargo-binstall")
        .tempdir_in(&install_path)
        .map_err(BinstallError::from)
        .wrap_err("Creating a temporary directory failed.")?;

    Ok((install_path, manifests, temp_dir))
}

/// Return vec of (crate_name, current_version)
fn filter_out_installed_crates(
    crate_names: Vec<CrateName>,
    force: bool,
    manifests: Option<&mut Manifests>,
) -> Result<impl Iterator<Item = (CrateName, Option<semver::Version>)> + '_> {
    let mut installed_crates = manifests
        .map(Manifests::load_installed_crates)
        .transpose()?;

    Ok(CrateName::dedup(crate_names)
    .filter_map(move |crate_name| {
        let name = &crate_name.name;

        let curr_version = installed_crates
            .as_mut()
            // Since crate_name is deduped, every entry of installed_crates
            // can be visited at most once.
            //
            // So here we take ownership of the version stored to avoid cloning.
            .and_then(|crates| crates.remove(name));

        match (
            force,
            curr_version,
            &crate_name.version_req,
        ) {
            (false, Some(curr_version), Some(version_req))
                if version_req.is_latest_compatible(&curr_version) =>
            {
                debug!("Bailing out early because we can assume wanted is already installed from metafile");
                info!("{name} v{curr_version} is already installed, use --force to override");
                None
            }

            // The version req is "*" thus a remote upgraded version could exist
            (false, Some(curr_version), None) => {
                Some((crate_name, Some(curr_version)))
            }

            _ => Some((crate_name, None)),
        }
    }))
}

#[allow(clippy::vec_box)]
fn do_install_fetches(
    resolution_fetchs: Vec<Box<ResolutionFetch>>,
    // Take manifests by value to drop the `FileLock`.
    manifests: Option<Manifests>,
    binstall_opts: &Options,
    dry_run: bool,
    temp_dir: tempfile::TempDir,
    no_cleanup: bool,
) -> Result<()> {
    if resolution_fetchs.is_empty() {
        return Ok(());
    }

    if dry_run {
        info!("Dry-run: Not proceeding to install fetched binaries");
        return Ok(());
    }

    block_in_place(|| {
        let metadata_vec = resolution_fetchs
            .into_iter()
            .map(|fetch| fetch.install(binstall_opts))
            .collect::<Result<Vec<_>, BinstallError>>()?;

        if let Some(manifests) = manifests {
            manifests.update(metadata_vec)?;
        }

        if no_cleanup {
            // Consume temp_dir without removing it from fs.
            let _ = temp_dir.into_path();
        } else {
            temp_dir.close().unwrap_or_else(|err| {
                warn!("Failed to clean up some resources: {err}");
            });
        }

        Ok(())
    })
}

#[allow(clippy::vec_box)]
fn do_install_fetches_continue_on_failure(
    resolution_fetchs: Vec<Box<ResolutionFetch>>,
    // Take manifests by value to drop the `FileLock`.
    manifests: Option<Manifests>,
    binstall_opts: &Options,
    dry_run: bool,
    temp_dir: tempfile::TempDir,
    no_cleanup: bool,
    errors: &mut Vec<Box<CrateContextError>>,
) -> Result<()> {
    if resolution_fetchs.is_empty() {
        return Ok(());
    }

    if dry_run {
        info!("Dry-run: Not proceeding to install fetched binaries");
        return Ok(());
    }

    block_in_place(|| {
        let metadata_vec = resolution_fetchs
            .into_iter()
            .filter_map(|fetch| match fetch.install(binstall_opts) {
                Ok(crate_info) => Some(crate_info),
                Err(BinstallError::CrateContext(err)) => {
                    errors.push(err);
                    None
                }
                Err(e) => panic!("Expected BinstallError::CrateContext(_), got {}", e),
            })
            .collect::<Vec<_>>();

        if let Some(manifests) = manifests {
            manifests.update(metadata_vec)?;
        }

        if no_cleanup {
            // Consume temp_dir without removing it from fs.
            let _ = temp_dir.into_path();
        } else {
            temp_dir.close().unwrap_or_else(|err| {
                warn!("Failed to clean up some resources: {err}");
            });
        }

        Ok(())
    })
}
