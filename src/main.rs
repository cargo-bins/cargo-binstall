use std::{
    ffi::OsString,
    mem::take,
    path::{Path, PathBuf},
    process::{ExitCode, Termination},
    sync::Arc,
    time::{Duration, Instant},
};

use clap::{AppSettings, Parser};
use log::{debug, error, info, warn, LevelFilter};
use miette::{miette, Result, WrapErr};
use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};
use tokio::{runtime::Runtime, task::block_in_place};

use cargo_binstall::{binstall, *};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[clap(version, about = "Install a Rust binary... from binaries!", setting = AppSettings::ArgRequiredElseHelp)]
struct Options {
    /// Package name for installation.
    ///
    /// This must be a crates.io package name.
    #[clap(value_name = "crate")]
    crate_names: Vec<CrateName>,

    /// Semver filter to select the package version to install.
    ///
    /// This is in Cargo.toml dependencies format: `--version 1.2.3` is equivalent to
    /// `--version "^1.2.3"`. Use `=1.2.3` to install a specific version.
    #[clap(long)]
    version: Option<String>,

    /// Override binary target set.
    ///
    /// Binstall is able to look for binaries for several targets, installing the first one it finds
    /// in the order the targets were given. For example, on a 64-bit glibc Linux distribution, the
    /// default is to look first for a `x86_64-unknown-linux-gnu` binary, then for a
    /// `x86_64-unknown-linux-musl` binary. However, on a musl system, the gnu version will not be
    /// considered.
    ///
    /// This option takes a comma-separated list of target triples, which will be tried in order.
    /// They override the default list, which is detected automatically from the current platform.
    ///
    /// If falling back to installing from source, the first target will be used.
    #[clap(
        help_heading = "OVERRIDES",
        alias = "target",
        long,
        value_name = "TRIPLE"
    )]
    targets: Option<String>,

    /// Override install path for downloaded binary.
    ///
    /// Defaults to `$HOME/.cargo/bin`
    #[clap(help_heading = "OVERRIDES", long)]
    install_path: Option<String>,

    /// Disable symlinking / versioned updates.
    ///
    /// By default, Binstall will install a binary named `<name>-<version>` in the install path, and
    /// either symlink or copy it to (depending on platform) the plain binary name. This makes it
    /// possible to have multiple versions of the same binary, for example for testing or rollback.
    ///
    /// Pass this flag to disable this behavior.
    #[clap(long)]
    no_symlinks: bool,

    /// Dry run, fetch and show changes without installing binaries.
    #[clap(long)]
    dry_run: bool,

    /// Disable interactive mode / confirmation prompts.
    #[clap(long)]
    no_confirm: bool,

    /// Do not cleanup temporary files.
    #[clap(long)]
    no_cleanup: bool,

    /// Enforce downloads over secure transports only.
    ///
    /// Insecure HTTP downloads will be removed completely in the future; in the meantime this
    /// option forces a fail when the remote endpoint uses plaintext HTTP or insecure TLS suites.
    ///
    /// Without this option, plain HTTP will warn.
    ///
    /// Implies `--min-tls-version=1.2`.
    #[clap(long)]
    secure: bool,

    /// Require a minimum TLS version from remote endpoints.
    ///
    /// The default is not to require any minimum TLS version, and use the negotiated highest
    /// version available to both this client and the remote server.
    #[clap(long, arg_enum, value_name = "VERSION")]
    min_tls_version: Option<TLSVersion>,

    /// Override manifest source.
    ///
    /// This skips searching crates.io for a manifest and uses the specified path directly, useful
    /// for debugging and when adding Binstall support. This must be the path to the folder
    /// containing a Cargo.toml file, not the Cargo.toml file itself.
    #[clap(help_heading = "OVERRIDES", long)]
    manifest_path: Option<PathBuf>,

    /// Utility log level
    ///
    /// Set to `debug` when submitting a bug report.
    #[clap(long, default_value = "info", value_name = "LEVEL")]
    log_level: LevelFilter,

    /// Override Cargo.toml package manifest bin-dir.
    #[clap(help_heading = "OVERRIDES", long)]
    bin_dir: Option<String>,

    /// Override Cargo.toml package manifest pkg-fmt.
    #[clap(help_heading = "OVERRIDES", long)]
    pkg_fmt: Option<PkgFmt>,

    /// Override Cargo.toml package manifest pkg-url.
    #[clap(help_heading = "OVERRIDES", long)]
    pkg_url: Option<String>,
}

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
    let handle = rt.spawn(entry(jobserver_client));
    let result = rt.block_on(handle);
    drop(rt);

    let done = start.elapsed();
    debug!("run time: {done:?}");

    result.map_or_else(
        |join_err| MainExit::Error(BinstallError::from(join_err)),
        |res| {
            res.map(|_| MainExit::Success(done)).unwrap_or_else(|err| {
                err.downcast::<BinstallError>()
                    .map(MainExit::Error)
                    .unwrap_or_else(MainExit::Report)
            })
        },
    )
}

async fn entry(jobserver_client: LazyJobserverClient) -> Result<()> {
    // Filter extraneous arg when invoked by cargo
    // `cargo run -- --help` gives ["target/debug/cargo-binstall", "--help"]
    // `cargo binstall --help` gives ["/home/ryan/.cargo/bin/cargo-binstall", "binstall", "--help"]
    let mut args: Vec<OsString> = std::env::args_os().collect();
    let args = if args.len() > 1 && args[1] == "binstall" {
        // Equivalent to
        //
        //     args.remove(1);
        //
        // But is O(1)
        args.swap(0, 1);
        let mut args = args.into_iter();
        drop(args.next().unwrap());

        args
    } else {
        args.into_iter()
    };

    // Load options
    let mut opts = Options::parse_from(args);
    let cli_overrides = PkgOverride {
        pkg_url: opts.pkg_url.take(),
        pkg_fmt: opts.pkg_fmt.take(),
        bin_dir: opts.bin_dir.take(),
    };
    let crate_names = take(&mut opts.crate_names);
    if crate_names.len() > 1 && opts.manifest_path.is_some() {
        return Err(BinstallError::ManifestPathConflictedWithBatchInstallation.into());
    }

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

    // Launch target detection
    let desired_targets = get_desired_targets(&opts.targets);

    // Compute install directory
    let (install_path, custom_install_path) = get_install_path(opts.install_path.as_deref());
    let install_path = install_path.ok_or_else(|| {
        error!("No viable install path found of specified, try `--install-path`");
        miette!("No install path found or specified")
    })?;
    debug!("Using install path: {}", install_path.display());

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

    let temp_dir_path: Arc<Path> = Arc::from(temp_dir.path());

    // Create binstall_opts
    let binstall_opts = Arc::new(binstall::Options {
        no_symlinks: opts.no_symlinks,
        dry_run: opts.dry_run,
        version: opts.version.take(),
        manifest_path: opts.manifest_path.take(),
        cli_overrides,
        desired_targets,
    });

    let tasks: Vec<_> = if !opts.dry_run && !opts.no_confirm {
        // Resolve crates
        let tasks: Vec<_> = crate_names
            .into_iter()
            .map(|crate_name| {
                tokio::spawn(binstall::resolve(
                    binstall_opts.clone(),
                    crate_name,
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
            resolutions.push(await_task(task).await?);
        }

        uithread.confirm().await?;

        // Install
        resolutions
            .into_iter()
            .map(|resolution| {
                tokio::spawn(binstall::install(
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
            .map(|crate_name| {
                let opts = binstall_opts.clone();
                let temp_dir_path = temp_dir_path.clone();
                let jobserver_client = jobserver_client.clone();
                let client = client.clone();
                let crates_io_api_client = crates_io_api_client.clone();
                let install_path = install_path.clone();

                tokio::spawn(async move {
                    let resolution = binstall::resolve(
                        opts.clone(),
                        crate_name,
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
        if let Some(metadata) = await_task(task).await? {
            metadata_vec.push(metadata);
        }
    }

    block_in_place(|| {
        if !custom_install_path {
            debug!("Writing .crates.toml");
            metafiles::v1::CratesToml::append(metadata_vec.iter())?;
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
