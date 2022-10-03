use std::{fs, path::Path, sync::Arc, time::Duration};

use binstalk::{
    errors::BinstallError,
    get_desired_targets,
    helpers::{jobserver_client::LazyJobserverClient, remote::Client, tasks::AutoAbortJoinHandle},
    manifests::{
        binstall_crates_v1::Records, cargo_crates_v1::CratesToml, cargo_toml_binstall::PkgOverride,
    },
    ops::{
        self,
        resolve::{CrateName, Resolution, VersionReqExt},
    },
};
use log::{debug, error, info, warn, LevelFilter};
use miette::{miette, Result, WrapErr};
use tokio::task::block_in_place;

use crate::{args::Args, install_path, ui::UIThread};

/// The time to delay for tasks resolving crates.
const TASK_DELAY: Duration = Duration::from_millis(200);

pub async fn install_crates(mut args: Args, jobserver_client: LazyJobserverClient) -> Result<()> {
    let cli_overrides = PkgOverride {
        pkg_url: args.pkg_url.take(),
        pkg_fmt: args.pkg_fmt.take(),
        bin_dir: args.bin_dir.take(),
    };

    // Launch target detection
    let desired_targets = get_desired_targets(args.targets.take());

    // Initialize reqwest client
    let client = Client::new(args.min_tls_version.map(|v| v.into()), TASK_DELAY)?;

    // Build crates.io api client
    let crates_io_api_client = crates_io_api::AsyncClient::with_http_client(
        client.get_inner().clone(),
        Duration::from_millis(100),
    );

    // Initialize UI thread
    let mut uithread = UIThread::new(!args.no_confirm);

    let (install_path, cargo_roots, metadata, temp_dir) = block_in_place(|| -> Result<_> {
        // Compute cargo_roots
        let cargo_roots =
            install_path::get_cargo_roots_path(args.roots.take()).ok_or_else(|| {
                error!("No viable cargo roots path found of specified, try `--roots`");
                miette!("No cargo roots path found or specified")
            })?;

        // Compute install directory
        let (install_path, custom_install_path) =
            install_path::get_install_path(args.install_path.as_deref(), Some(&cargo_roots));
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

    let temp_dir_path: Arc<Path> = Arc::from(temp_dir.path());

    // Create binstall_opts
    let binstall_opts = Arc::new(ops::Options {
        no_symlinks: args.no_symlinks,
        dry_run: args.dry_run,
        force: args.force,
        version_req: args.version_req.take(),
        manifest_path: args.manifest_path.take(),
        cli_overrides,
        desired_targets,
        quiet: args.log_level == LevelFilter::Off,
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
                    temp_dir_path.clone(),
                    install_path.clone(),
                    client.clone(),
                    crates_io_api_client.clone(),
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

        uithread.confirm().await?;

        // Install
        resolutions
            .into_iter()
            .map(|resolution| {
                AutoAbortJoinHandle::spawn(ops::install::install(
                    resolution,
                    binstall_opts.clone(),
                    jobserver_client.clone(),
                ))
            })
            .collect()
    } else {
        // Resolve crates and install without confirmation
        crate_names
            .into_iter()
            .map(|(crate_name, current_version)| {
                let opts = binstall_opts.clone();
                let temp_dir_path = temp_dir_path.clone();
                let jobserver_client = jobserver_client.clone();
                let client = client.clone();
                let crates_io_api_client = crates_io_api_client.clone();
                let install_path = install_path.clone();

                AutoAbortJoinHandle::spawn(async move {
                    let resolution = ops::resolve::resolve(
                        opts.clone(),
                        crate_name,
                        current_version,
                        temp_dir_path,
                        install_path,
                        client,
                        crates_io_api_client,
                    )
                    .await?;

                    ops::install::install(resolution, opts, jobserver_client).await
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
