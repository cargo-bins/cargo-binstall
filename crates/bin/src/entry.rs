use std::{collections::BTreeMap, fs, io, path::PathBuf, sync::Arc, time::Duration};

use binstalk::{
    errors::BinstallError,
    fetchers::{Fetcher, GhCrateMeta, QuickInstall},
    get_desired_targets,
    helpers::{jobserver_client::LazyJobserverClient, remote::Client, tasks::AutoAbortJoinHandle},
    ops::{
        self,
        resolve::{CrateName, Resolution, VersionReqExt},
        Resolver,
    },
};
use binstalk_manifests::{
    binstall_crates_v1::Records as BinstallCratesV1Records,
    cargo_crates_v1::{CratesToml, CratesTomlParseError},
    cargo_toml_binstall::PkgOverride,
    CompactString, Version,
};
use crates_io_api::AsyncClient as CratesIoApiClient;
use log::LevelFilter;
use miette::{miette, Result, WrapErr};
use tokio::task::block_in_place;
use tracing::{debug, error, info, warn};

use crate::{
    args::{Args, Strategy},
    install_path,
    ui::confirm,
};

pub async fn install_crates(args: Args, jobserver_client: LazyJobserverClient) -> Result<()> {
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
    let (install_path, cargo_roots, metadata, temp_dir) =
        compute_paths_and_load_manifests(args.roots, args.install_path)?;

    // Remove installed crates
    let mut crate_names =
        filter_out_installed_crates(args.crate_names, args.force, metadata.as_ref()).peekable();

    if crate_names.peek().is_none() {
        debug!("Nothing to do");
        return Ok(());
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
    )
    .map_err(BinstallError::from)?;

    // Build crates.io api client
    let crates_io_api_client =
        CratesIoApiClient::with_http_client(client.get_inner().clone(), Duration::from_millis(100));

    // Create binstall_opts
    let binstall_opts = Arc::new(ops::Options {
        no_symlinks: args.no_symlinks,
        dry_run: args.dry_run,
        force: args.force,
        quiet: args.log_level == LevelFilter::Off,

        version_req: args.version_req,
        manifest_path: args.manifest_path,
        cli_overrides,

        desired_targets,
        resolvers,
        cargo_install_fallback,

        temp_dir: temp_dir.path().to_owned(),
        install_path,
        client,
        crates_io_api_client,
        jobserver_client,
    });

    // Destruct args before any async function to reduce size of the future
    let dry_run = args.dry_run;
    let no_confirm = args.no_confirm;
    let no_cleanup = args.no_cleanup;

    let tasks: Vec<_> = if !dry_run && !no_confirm {
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

        // Confirm
        let mut resolutions = Vec::with_capacity(tasks.len());
        for task in tasks {
            match task.await?? {
                Resolution::AlreadyUpToDate => {}
                res => resolutions.push(res),
            }
        }

        if resolutions.is_empty() {
            debug!("Nothing to do");
            return Ok(());
        }

        confirm().await?;

        // Install
        resolutions
            .into_iter()
            .map(|resolution| {
                AutoAbortJoinHandle::spawn(ops::install::install(resolution, binstall_opts.clone()))
            })
            .collect()
    } else {
        // Resolve crates and install without confirmation
        crate_names
            .map(|(crate_name, current_version)| {
                let opts = binstall_opts.clone();

                AutoAbortJoinHandle::spawn(async move {
                    let resolution =
                        ops::resolve::resolve(opts.clone(), crate_name, current_version).await?;

                    ops::install::install(resolution, opts).await
                })
            })
            .collect()
    };

    let mut metadata_vec = Vec::with_capacity(tasks.len());
    for task in tasks {
        if let Some(metadata) = task.await?? {
            metadata_vec.push(metadata);
        }
    }

    block_in_place(|| {
        if let Some((mut cargo_binstall_metadata, _)) = metadata {
            // The cargo manifest path is already created when loading
            // metadata.

            debug!("Writing .crates.toml");
            CratesToml::append_to_path(cargo_roots.join(".crates.toml"), metadata_vec.iter())?;

            debug!("Writing binstall/crates-v1.json");
            for metadata in metadata_vec {
                cargo_binstall_metadata.replace(metadata);
            }
            cargo_binstall_metadata.overwrite()?;
        }

        if no_cleanup {
            // Consume temp_dir without removing it from fs.
            temp_dir.into_path();
        } else {
            temp_dir.close().unwrap_or_else(|err| {
                warn!("Failed to clean up some resources: {err}");
            });
        }

        Ok(())
    })
}

type Metadata = (BinstallCratesV1Records, BTreeMap<CompactString, Version>);

/// Return (install_path, cargo_roots, metadata, temp_dir)
fn compute_paths_and_load_manifests(
    roots: Option<PathBuf>,
    install_path: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf, Option<Metadata>, tempfile::TempDir)> {
    block_in_place(|| {
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

        // Load metadata
        let metadata = if !custom_install_path {
            // Read cargo_binstall_metadata
            let metadata_dir = cargo_roots.join("binstall");
            fs::create_dir_all(&metadata_dir).map_err(BinstallError::Io)?;
            let manifest_path = metadata_dir.join("crates-v1.json");

            debug!(
                "Reading {} from {}",
                "cargo_binstall_metadata",
                manifest_path.display()
            );

            let cargo_binstall_metadata = BinstallCratesV1Records::load_from_path(&manifest_path)?;

            // Read cargo_install_v1_metadata
            let manifest_path = cargo_roots.join(".crates.toml");

            debug!(
                "Reading {} from {}",
                "cargo_install_v1_metadata",
                manifest_path.display()
            );

            let cargo_install_v1_metadata = match CratesToml::load_from_path(&manifest_path) {
                Ok(metadata) => metadata.collect_into_crates_versions()?,
                Err(CratesTomlParseError::Io(io_err))
                    if io_err.kind() == io::ErrorKind::NotFound =>
                {
                    // .crates.toml does not exist, create an empty BTreeMap
                    Default::default()
                }
                Err(err) => Err(err)?,
            };

            Some((cargo_binstall_metadata, cargo_install_v1_metadata))
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

        Ok((install_path, cargo_roots, metadata, temp_dir))
    })
}

/// Return vec of (crate_name, current_version)
fn filter_out_installed_crates(
    crate_names: Vec<CrateName>,
    force: bool,
    metadata: Option<&Metadata>,
) -> impl Iterator<Item = (CrateName, Option<semver::Version>)> + '_ {
    CrateName::dedup(crate_names)
    .filter_map(move |crate_name| {
        let name = &crate_name.name;

        let curr_version = metadata
            .and_then(|metadata| {
                metadata
                    .0
                    .get(name)
                    .map(|crate_info| &crate_info.current_version)
                    .into_iter()
                    .chain(metadata.1.get(name))
                    // Since the cargo_install_v1_metadata could be out of sync
                    // from cargo_binstall_metadata, it is better to obtain
                    // the version from both of them and takes the larger one.
                    .max()
            });

        match (
            force,
            curr_version,
            &crate_name.version_req,
        ) {
            (false, Some(curr_version), Some(version_req))
                if version_req.is_latest_compatible(curr_version) =>
            {
                debug!("Bailing out early because we can assume wanted is already installed from metafile");
                info!("{name} v{curr_version} is already installed, use --force to override");
                None
            }

            // The version req is "*" thus a remote upgraded version could exist
            (false, Some(curr_version), None) => {
                Some((crate_name, Some(curr_version.clone())))
            }

            _ => Some((crate_name, None)),
        }
    })
}
