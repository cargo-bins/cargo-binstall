use std::{
    env, fs,
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use binstalk::{
    errors::BinstallError,
    fetchers::{Fetcher, GhCrateMeta, QuickInstall},
    get_desired_targets,
    helpers::{
        gh_api_client::GhApiClient,
        jobserver_client::LazyJobserverClient,
        remote::{Certificate, Client},
        tasks::AutoAbortJoinHandle,
    },
    ops::{
        self,
        resolve::{CrateName, Resolution, ResolutionFetch, VersionReqExt},
        CargoTomlFetchOverride, Options, Resolver,
    },
};
use binstalk_manifests::cargo_toml_binstall::PkgOverride;
use file_format::FileFormat;
use log::LevelFilter;
use miette::{miette, Result, WrapErr};
use tokio::task::block_in_place;
use tracing::{debug, error, info, warn};

use crate::{
    args::{Args, Strategy},
    install_path,
    manifests::Manifests,
    ui::confirm,
};

pub fn install_crates(
    args: Args,
    jobserver_client: LazyJobserverClient,
) -> Result<Option<impl Future<Output = Result<()>>>> {
    // Compute Resolvers
    let mut cargo_install_fallback = false;

    let resolvers: Vec<_> = args
        .strategies
        .into_iter()
        .filter_map(|strategy| match strategy {
            Strategy::CrateMetaData => Some(GhCrateMeta::new as Resolver),
            Strategy::QuickInstall => Some(QuickInstall::new as Resolver),
            Strategy::Compile => {
                cargo_install_fallback = true;
                None
            }
        })
        .collect();

    // Compute paths
    let cargo_root = args.root;
    let (install_path, mut manifests, temp_dir) =
        compute_paths_and_load_manifests(cargo_root.clone(), args.install_path, args.no_track)?;

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
        pkg_url: args.pkg_url,
        pkg_fmt: args.pkg_fmt,
        bin_dir: args.bin_dir,
    };

    // Initialize reqwest client
    let rate_limit = args.rate_limit;

    let client = Client::new(
        concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")),
        args.min_tls_version.map(|v| v.into()),
        Duration::from_millis(rate_limit.duration.get()),
        rate_limit.request_count,
        read_root_certs(args.root_certificates),
    )
    .map_err(BinstallError::from)?;

    let gh_api_client = GhApiClient::new(client.clone(), args.github_token);

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
        registry: args.index.unwrap_or_default(),
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

    Ok(Some(async move {
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

fn read_root_certs(root_certificate_paths: Vec<PathBuf>) -> impl Iterator<Item = Certificate> {
    root_certificate_paths
        .into_iter()
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
) -> Result<(PathBuf, Option<Manifests>, tempfile::TempDir)> {
    // Compute cargo_roots
    let cargo_roots = install_path::get_cargo_roots_path(roots).ok_or_else(|| {
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
