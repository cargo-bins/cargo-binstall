use std::{fs, mem, sync::Arc, time::Duration};

use binstalk::{
    errors::BinstallError,
    fetchers::{Fetcher, GhCrateMeta, QuickInstall},
    get_desired_targets,
    helpers::{jobserver_client::LazyJobserverClient, remote::Client, tasks::AutoAbortJoinHandle},
    ops::{
        self,
        resolve::{CrateName, Resolution, VersionReqExt},
    },
};
use binstalk_manifests::{
    binstall_crates_v1::Records, cargo_crates_v1::CratesToml, cargo_toml_binstall::PkgOverride,
};
use crates_io_api::AsyncClient as CratesIoApiClient;
use log::LevelFilter;
use miette::{miette, Result, WrapErr};
use tokio::task::block_in_place;
use tracing::{debug, error, info, warn};

use crate::{
    args::{Args, Strategy},
    install_path,
    ui::UIThread,
};

pub async fn install_crates(mut args: Args, jobserver_client: LazyJobserverClient) -> Result<()> {
    // Launch target detection
    let desired_targets = get_desired_targets(args.targets.take());

    // Compute strategies
    let mut strategies = vec![];

    // Remove duplicate strategies
    for strategy in mem::take(&mut args.strategies) {
        if !strategies.contains(&strategy) {
            strategies.push(strategy);
        }
    }

    // Default strategies if empty
    if strategies.is_empty() {
        strategies = vec![
            Strategy::CrateMetaData,
            Strategy::QuickInstall,
            Strategy::Compile,
        ];
    }

    let disable_strategies = mem::take(&mut args.disable_strategies);

    let mut strategies: Vec<Strategy> = if !disable_strategies.is_empty() {
        strategies
            .into_iter()
            .filter(|strategy| !disable_strategies.contains(strategy))
            .collect()
    } else {
        strategies
    };

    if strategies.is_empty() {
        return Err(BinstallError::InvalidStrategies(&"No strategy is provided").into());
    }

    let cargo_install_fallback = *strategies.last().unwrap() == Strategy::Compile;

    if cargo_install_fallback {
        strategies.pop().unwrap();
    }

    let resolvers: Vec<_> = strategies
        .into_iter()
        .map(|strategy| match strategy {
            Strategy::CrateMetaData => GhCrateMeta::new,
            Strategy::QuickInstall => QuickInstall::new,
            Strategy::Compile => unreachable!(),
        })
        .collect();

    let cli_overrides = PkgOverride {
        pkg_url: args.pkg_url.take(),
        pkg_fmt: args.pkg_fmt.take(),
        bin_dir: args.bin_dir.take(),
    };

    let rate_limit = args.rate_limit;

    // Initialize reqwest client
    let client = Client::new(
        args.min_tls_version.map(|v| v.into()),
        Duration::from_millis(rate_limit.duration.get()),
        rate_limit.request_count,
    )
    .map_err(BinstallError::from)?;

    // Build crates.io api client
    let crates_io_api_client =
        CratesIoApiClient::with_http_client(client.get_inner().clone(), Duration::from_millis(100));

    let (install_path, cargo_roots, metadata, temp_dir) = block_in_place(|| -> Result<_> {
        // Compute cargo_roots
        let cargo_roots =
            install_path::get_cargo_roots_path(args.roots.take()).ok_or_else(|| {
                error!("No viable cargo roots path found of specified, try `--roots`");
                miette!("No cargo roots path found or specified")
            })?;

        // Compute install directory
        let (install_path, custom_install_path) =
            install_path::get_install_path(args.install_path.take(), Some(&cargo_roots));
        let install_path = install_path.ok_or_else(|| {
            error!("No viable install path found of specified, try `--install-path`");
            miette!("No install path found or specified")
        })?;
        fs::create_dir_all(&install_path).map_err(BinstallError::Io)?;
        debug!("Using install path: {}", install_path.display());

        // Load metadata
        let metadata = if !custom_install_path {
            let metadata_dir = cargo_roots.join("binstall");
            fs::create_dir_all(&metadata_dir).map_err(BinstallError::Io)?;
            let manifest_path = metadata_dir.join("crates-v1.json");

            debug!("Reading {}", manifest_path.display());
            Some(Records::load_from_path(&manifest_path)?)
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
    })?;

    // Remove installed crates
    let crate_names = CrateName::dedup(&args.crate_names)
    .filter_map(|crate_name| {
        match (
            args.force,
            metadata.as_ref().and_then(|records| records.get(&crate_name.name)),
            &crate_name.version_req,
        ) {
            (false, Some(metadata), Some(version_req))
                if version_req.is_latest_compatible(&metadata.current_version) =>
            {
                debug!("Bailing out early because we can assume wanted is already installed from metafile");
                info!(
                    "{} v{} is already installed, use --force to override",
                    crate_name.name, metadata.current_version
                );
                None
            }

            // we have to assume that the version req could be *,
            // and therefore a remote upgraded version could exist
            (false, Some(metadata), _) => {
                Some((crate_name, Some(metadata.current_version.clone())))
            }

            _ => Some((crate_name, None)),
        }
    })
    .collect::<Vec<_>>();

    if crate_names.is_empty() {
        debug!("Nothing to do");
        return Ok(());
    }

    // Create binstall_opts
    let binstall_opts = Arc::new(ops::Options {
        no_symlinks: args.no_symlinks,
        dry_run: args.dry_run,
        force: args.force,
        quiet: args.log_level == LevelFilter::Off,

        version_req: args.version_req.take(),
        manifest_path: args.manifest_path.take(),
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

    let tasks: Vec<_> = if !args.dry_run && !args.no_confirm {
        // Resolve crates
        let tasks: Vec<_> = crate_names
            .into_iter()
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

        // Initialize UI thread
        UIThread::new(!args.no_confirm).confirm().await?;

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
            .into_iter()
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
        if let Some(mut records) = metadata {
            // The cargo manifest path is already created when loading
            // metadata.

            debug!("Writing .crates.toml");
            CratesToml::append_to_path(cargo_roots.join(".crates.toml"), metadata_vec.iter())?;

            debug!("Writing binstall/crates-v1.json");
            for metadata in metadata_vec {
                records.replace(metadata);
            }
            records.overwrite()?;
        }

        if args.no_cleanup {
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
