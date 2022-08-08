use std::{
    fs,
    mem::take,
    path::Path,
    process::{ExitCode, Termination},
    sync::Arc,
    time::{Duration, Instant},
};

use log::{debug, error, info, warn, LevelFilter};
use miette::{miette, Result, WrapErr};
use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};
use tokio::{runtime::Runtime, task::block_in_place};

use cargo_binstall::{binstall, *};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

enum MainExit {
    Success(Duration),
    Error(BinstallError),
    Report(miette::Report),
}

impl Termination for MainExit {
    fn report(self) -> ExitCode {
        match self {
            Self::Success(spent) => {
                info!("Installation completed in {spent:?}");
                ExitCode::SUCCESS
            }
            Self::Error(err) => err.report(),
            Self::Report(err) => {
                error!("Fatal error:");
                eprintln!("{err:?}");
                ExitCode::from(16)
            }
        }
    }
}

fn main() -> MainExit {
    // Create jobserver client
    let jobserver_client = LazyJobserverClient::new();

    let start = Instant::now();

    let rt = Runtime::new().unwrap();
    let handle = AutoAbortJoinHandle::new(rt.spawn(entry(jobserver_client)));
    let result = rt.block_on(cancel_on_user_sig_term(handle));
    drop(rt);

    let done = start.elapsed();
    debug!("run time: {done:?}");

    result.map_or_else(MainExit::Error, |res| {
        res.map(|()| MainExit::Success(done)).unwrap_or_else(|err| {
            err.downcast::<BinstallError>()
                .map(MainExit::Error)
                .unwrap_or_else(MainExit::Report)
        })
    })
}

async fn entry(jobserver_client: LazyJobserverClient) -> Result<()> {
    let mut opts = Options::parse()?;

    let crate_names = take(&mut opts.crate_names);

    let cli_overrides = PkgOverride {
        pkg_url: opts.pkg_url.take(),
        pkg_fmt: opts.pkg_fmt.take(),
        bin_dir: opts.bin_dir.take(),
    };

    // Launch target detection
    let desired_targets = get_desired_targets(&opts.targets);

    // Initialize reqwest client
    let client = create_reqwest_client(opts.secure, opts.min_tls_version.map(|v| v.into()))?;

    // Build crates.io api client
    let crates_io_api_client = crates_io_api::AsyncClient::new(
        "cargo-binstall (https://github.com/ryankurte/cargo-binstall)",
        Duration::from_millis(100),
    )
    .expect("bug: invalid user agent");

    // Setup logging
    let mut log_config = ConfigBuilder::new();
    log_config.add_filter_ignore("hyper".to_string());
    log_config.add_filter_ignore("reqwest".to_string());
    log_config.add_filter_ignore("rustls".to_string());
    log_config.set_location_level(LevelFilter::Off);
    TermLogger::init(
        opts.log_level,
        log_config.build(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .unwrap();

    // Initialize UI thread
    let mut uithread = UIThread::new(!opts.no_confirm);

    let (install_path, metadata, temp_dir) = block_in_place(|| -> Result<_> {
        // Compute install directory
        let (install_path, custom_install_path) = get_install_path(opts.install_path.as_deref());
        let install_path = install_path.ok_or_else(|| {
            error!("No viable install path found of specified, try `--install-path`");
            miette!("No install path found or specified")
        })?;
        fs::create_dir_all(&install_path).map_err(BinstallError::Io)?;
        debug!("Using install path: {}", install_path.display());

        // Load metadata
        let metadata = if !custom_install_path {
            debug!("Reading binstall/crates-v1.json");
            Some(metafiles::binstall_v1::Records::load()?)
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

        Ok((install_path, metadata, temp_dir))
    })?;

    // Remove installed crates
    let crate_names = CrateName::dedup(crate_names).filter_map(|crate_name| {
        if opts.force {
            Some((crate_name, None))
        } else if let Some(records) = &metadata {
            if let Some(metadata) = records.get(&crate_name.name) {
                if let Some(version_req) = &crate_name.version_req {
                    if version_req.is_latest_compatible(&metadata.current_version) {
                        info!(
                            "package {crate_name} is already installed and cannot be upgraded, use --force to override"
                        );
                        None
                    } else {
                        Some((crate_name, Some(metadata.current_version.clone())))
                    }
                } else {
                    info!("package {crate_name} is already installed, use --force to override");
                    None
                }
            } else {
                Some((crate_name, None))
            }
        } else {
            Some((crate_name, None))
        }
    });

    let temp_dir_path: Arc<Path> = Arc::from(temp_dir.path());

    // Create binstall_opts
    let binstall_opts = Arc::new(binstall::Options {
        no_symlinks: opts.no_symlinks,
        dry_run: opts.dry_run,
        force: opts.force,
        version_req: opts.version_req.take(),
        manifest_path: opts.manifest_path.take(),
        cli_overrides,
        desired_targets,
        quiet: opts.log_level == LevelFilter::Off,
    });

    let tasks: Vec<_> = if !opts.dry_run && !opts.no_confirm {
        // Resolve crates
        let tasks: Vec<_> = crate_names
            .into_iter()
            .map(|(crate_name, current_version)| {
                AutoAbortJoinHandle::spawn(binstall::resolve(
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
            resolutions.push(task.await??);
        }

        uithread.confirm().await?;

        // Install
        resolutions
            .into_iter()
            .map(|resolution| {
                AutoAbortJoinHandle::spawn(binstall::install(
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
                    let resolution = binstall::resolve(
                        opts.clone(),
                        crate_name,
                        current_version,
                        temp_dir_path,
                        install_path,
                        client,
                        crates_io_api_client,
                    )
                    .await?;

                    binstall::install(resolution, opts, jobserver_client).await
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
            // If using standardised install path,
            // then create_dir_all(&install_path) would also
            // create .cargo.

            debug!("Writing .crates.toml");
            metafiles::v1::CratesToml::append(metadata_vec.iter())?;

            debug!("Writing binstall/crates-v1.json");
            for metadata in metadata_vec {
                records.replace(metadata);
            }
            records.overwrite()?;
        }

        if opts.no_cleanup {
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
