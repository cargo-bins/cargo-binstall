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
    /// Packages to install.
    ///
    /// Syntax: crate[@version]
    ///
    /// Each value is either a crate name alone, or a crate name followed by @ and the version to
    /// install. The version syntax is as with the --version option.
    ///
    /// When multiple names are provided, the --version option and any override options are
    /// unavailable due to ambiguity.
    #[clap(help_heading = "Package selection", value_name = "crate[@version]")]
    crate_names: Vec<CrateName>,

    /// Package version to install.
    ///
    /// Takes either an exact semver version or a semver version requirement expression, which will
    /// be resolved to the highest matching version available.
    ///
    /// Cannot be used when multiple packages are installed at once, use the attached version
    /// syntax in that case.
    #[clap(help_heading = "Package selection", long = "version")]
    version_req: Option<String>,

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
        help_heading = "Package selection",
        alias = "target",
        long,
        value_name = "TRIPLE"
    )]
    targets: Option<String>,

    /// Override Cargo.toml package manifest path.
    ///
    /// This skips searching crates.io for a manifest and uses the specified path directly, useful
    /// for debugging and when adding Binstall support. This may be either the path to the folder
    /// containing a Cargo.toml file, or the Cargo.toml file itself.
    #[clap(help_heading = "Overrides", long)]
    manifest_path: Option<PathBuf>,

    /// Override Cargo.toml package manifest bin-dir.
    #[clap(help_heading = "Overrides", long)]
    bin_dir: Option<String>,

    /// Override Cargo.toml package manifest pkg-fmt.
    #[clap(help_heading = "Overrides", long)]
    pkg_fmt: Option<PkgFmt>,

    /// Override Cargo.toml package manifest pkg-url.
    #[clap(help_heading = "Overrides", long)]
    pkg_url: Option<String>,

    /// Disable versioned updates.
    ///
    /// By default, Binstall will install a binary named `<name>-<version>` in the install path, and
    /// (depending on platform) either symlink or copy it to the plain binary name. This makes it
    /// possible to have multiple versions of the same binary, for example for testing or rollback.
    ///
    /// Pass this flag to disable this behavior.
    #[clap(help_heading = "Options", long, alias = "no-symlinks")]
    no_versioned: bool,

    /// Dry run, fetch and show changes without installing binaries.
    #[clap(help_heading = "Options", long)]
    dry_run: bool,

    /// Disable interactive mode / confirmation prompts.
    ///
    /// This is automatically set when the terminal is not interactive.
    #[clap(help_heading = "Options", long)]
    no_confirm: bool,

    /// Do not cleanup temporary files.
    #[clap(help_heading = "Options", long)]
    no_cleanup: bool,

    /// Install binaries in a custom location.
    ///
    /// By default, binaries are installed to the global location `$CARGO_HOME/bin`, and global
    /// metadata files are updated with the package information. Specifying another path here
    /// switches over to a "local" install, where binaries are installed at the path given, and the
    /// global metadata files are not updated.
    #[clap(help_heading = "Options", long)]
    install_path: Option<String>,

    /// Enforce downloads over secure transports only.
    ///
    /// Insecure HTTP downloads will be removed completely in the future; in the meantime this
    /// option forces a fail when the remote endpoint uses plaintext HTTP or insecure TLS suites.
    ///
    /// Without this option, plain HTTP will warn.
    ///
    /// Implies `--min-tls-version=1.2`.
    #[clap(help_heading = "Options", long)]
    secure: bool,

    /// Require a minimum TLS version from remote endpoints.
    ///
    /// The default is not to require any minimum TLS version, and use the negotiated highest
    /// version available to both this client and the remote server.
    #[clap(help_heading = "Options", long, arg_enum, value_name = "VERSION")]
    min_tls_version: Option<TLSVersion>,

    /// Print help information
    #[clap(help_heading = "Meta", short, long)]
    help: bool,

    /// Print version information
    #[clap(help_heading = "Meta", short = 'V')]
    version: bool,

    /// Show logs at this level
    ///
    /// Set to `debug` when submitting a bug report.
    #[clap(help_heading = "Meta", long, value_name = "LEVEL")]
    log_level: Option<LevelFilter>,
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

    let crate_names = take(&mut opts.crate_names);
    if crate_names.len() > 1 {
        let option = if opts.version_req.is_some() {
            "version"
        } else if opts.manifest_path.is_some() {
            "manifest-path"
        } else if opts.bin_dir.is_some() {
            "bin-dir"
        } else if opts.pkg_fmt.is_some() {
            "pkg-fmt"
        } else if opts.pkg_url.is_some() {
            "pkg-url"
        } else {
            ""
        };

        if option != "" {
            return Err(BinstallError::OverrideOptionUsedWithMultiInstall { option }.into());
        }
    }

    let cli_overrides = PkgOverride {
        pkg_url: opts.pkg_url.take(),
        pkg_fmt: opts.pkg_fmt.take(),
        bin_dir: opts.bin_dir.take(),
    };

    // Initialize reqwest client
    let client = create_reqwest_client(opts.secure, opts.min_tls_version.map(|v| v.into()))?;

    // Build crates.io api client
    let crates_io_api_client =
        crates_io_api::AsyncClient::new(USER_AGENT, Duration::from_millis(100))
            .expect("bug: invalid user agent");

    // Disable interactivity if there's no one to see it
    if !console::user_attended() {
        opts.log_level.get_or_insert(LevelFilter::Info);
        opts.no_confirm = true;
    }

    // Setup logging
    if let Some(level) = opts.log_level {
        let mut log_config = ConfigBuilder::new();
        log_config.add_filter_ignore("hyper".to_string());
        log_config.add_filter_ignore("reqwest".to_string());
        log_config.add_filter_ignore("rustls".to_string());
        log_config.set_location_level(LevelFilter::Off);
        TermLogger::init(
            level,
            log_config.build(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        )
        .unwrap();
    }

    // Initialize UI thread
    let (ui, confirm) = ui::init();
    ui.setup(3, "Prepare");

    // Launch target detection
    ui.doing("targets");
    let desired_targets = get_desired_targets(&opts.targets);
    ui.done("targets");

    // Compute install directory
    ui.doing("install path");
    let (install_path, custom_install_path) = get_install_path(opts.install_path.as_deref());
    let install_path = install_path.ok_or_else(|| {
        error!("No viable install path found of specified, try `--install-path`");
        miette!("No install path found or specified")
    })?;
    debug!("Using install path: {}", install_path.display());
    ui.done("install path");

    // Create a temporary directory for downloads etc.
    //
    // Put all binaries to a temporary directory under `dst` first, catching
    // some failure modes (e.g., out of space) before touching the existing
    // binaries. This directory will get cleaned up via RAII.
    ui.doing("temporary folder");
    let temp_dir = tempfile::Builder::new()
        .prefix("cargo-binstall")
        .tempdir_in(&install_path)
        .map_err(BinstallError::from)
        .wrap_err("Creating a temporary directory failed.")?;

    let temp_dir_path: Arc<Path> = Arc::from(temp_dir.path());
    ui.done("temporary folder");

    // Create binstall_opts
    let binstall_opts = Arc::new(binstall::Options {
        versioned: !opts.no_versioned,
        dry_run: opts.dry_run,
        version: opts.version_req.take(),
        manifest_path: opts.manifest_path.take(),
        cli_overrides,
        desired_targets,
    });

    let ctx = binstall::Context {
        opts: binstall_opts.clone(),
        temp_dir: temp_dir_path.clone(),
        install_path: install_path.clone(),
        client: client.clone(),
        crates_io_api_client: crates_io_api_client.clone(),
        jobserver_client: jobserver_client.clone(),
    };

    let mut results = Vec::with_capacity(crate_names.len());

    ui.setup(crate_names.len(), "Resolve");
    let tasks: Vec<_> = crate_names
        .into_iter()
        .map(|crate_name| {
            let ctx = ctx.clone();
            let ui = ui.clone();
            tokio::spawn(async move {
                let step = crate_name.to_string();
                ui.doing(&step);
                let res = binstall::resolve(ctx, crate_name).await;
                ui.done(&step);
                res
            })
        })
        .collect();

    // Gather info
    let mut resolutions = Vec::with_capacity(tasks.len());
    for task in tasks {
        resolutions.push(await_task(task).await?);
    }

    // Show summary of what's going to be installed
    ui.summary(&resolutions, binstall_opts.versioned);

    if opts.dry_run {
        ui.finish();
        return Ok(());
    }

    if !opts.no_confirm {
        confirm.confirm().await?;
    }

    // Install
    let (fetches, sources): (Vec<_>, Vec<_>) = resolutions.into_iter().partition(|res| match res {
        binstall::Resolution::Fetch { .. } => true,
        binstall::Resolution::Source { .. } => false,
    });

    if !fetches.is_empty() {
        ui.setup(fetches.len(), "Install");
        let tasks: Vec<_> = fetches
            .into_iter()
            .map(|fetch| {
                let ctx = ctx.clone();
                let ui = ui.clone();
                tokio::spawn(async move {
                    let step = fetch.name().to_string();
                    ui.doing(&step);
                    let res = binstall::install(ctx, fetch).await;
                    ui.done(&step);
                    res
                })
            })
            .collect();

        // Wait for all fetches to be done
        for task in tasks {
            if let Some(res) = await_task(task).await? {
                results.push(res);
            }
        }
    }

    if !sources.is_empty() {
        // Run source installs in series
        ui.setup(sources.len(), "Build");
        for src in sources {
            let step = src.name().to_string();
            ui.doing(&step);
            if let Some(res) = await_task(tokio::spawn(binstall::install(ctx.clone(), src))).await?
            {
                results.push(res);
                ui.done(&step);
            }
        }
    }

    block_in_place(|| {
        if !custom_install_path {
            ui.setup(2, "Register");

            debug!("Writing .crates.toml");
            ui.doing(".crates.toml");
            metafiles::v1::CratesToml::append(results.iter())?;
            ui.done(".crates.toml");

            debug!("Writing binstall/crates-v1.json");
            ui.doing("binstall/crates-v1.json");
            metafiles::binstall_v1::append(results)?;
            ui.done("binstall/crates-v1.json");
        }

        if opts.no_cleanup {
            // Consume temp_dir without removing it from fs.
            temp_dir.into_path();
        } else {
            temp_dir.close().unwrap_or_else(|err| {
                warn!("Failed to clean up some resources: {err}");
            });
        }

        ui.finish();
        Ok(())
    })
}
