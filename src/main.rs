use std::{
    collections::BTreeSet,
    ffi::OsString,
    mem::take,
    path::PathBuf,
    process::{ExitCode, Termination},
    sync::Arc,
    time::{Duration, Instant},
};

use cargo_toml::{Package, Product};
use clap::Parser;
use log::{debug, error, info, warn, LevelFilter};
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use reqwest::Client;
use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};
use tempfile::TempDir;
use tokio::{
    process::Command,
    runtime::Runtime,
    task::{block_in_place, JoinError},
};

use cargo_binstall::{
    bins,
    fetchers::{Data, Fetcher, GhCrateMeta, MultiFetcher, QuickInstall},
    *,
};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[clap(version, about = "Install a Rust binary... from binaries!")]
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
    JoinErr(JoinError),
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
            Self::JoinErr(err) => {
                error!("Fatal error:");
                eprintln!("{err:?}");
                ExitCode::from(17)
            }
        }
    }
}

fn main() -> MainExit {
    let start = Instant::now();

    let rt = Runtime::new().unwrap();
    let handle = rt.spawn(entry());
    let result = rt.block_on(handle);
    drop(rt);

    let done = start.elapsed();
    debug!("run time: {done:?}");

    result.map_or_else(MainExit::JoinErr, |res| {
        res.map(|_| MainExit::Success(done)).unwrap_or_else(|err| {
            err.downcast::<BinstallError>()
                .map(MainExit::Error)
                .unwrap_or_else(MainExit::Report)
        })
    })
}

async fn entry() -> Result<()> {
    // Filter extraneous arg when invoked by cargo
    // `cargo run -- --help` gives ["target/debug/cargo-binstall", "--help"]
    // `cargo binstall --help` gives ["/home/ryan/.cargo/bin/cargo-binstall", "binstall", "--help"]
    let mut args: Vec<OsString> = std::env::args_os().collect();
    if args.len() > 1 && args[1] == "binstall" {
        args.remove(1);
    }

    // Load options
    let mut opts = Options::parse_from(args);
    let cli_overrides = Arc::new(PkgOverride {
        pkg_url: opts.pkg_url.take(),
        pkg_fmt: opts.pkg_fmt.take(),
        bin_dir: opts.bin_dir.take(),
    });
    let crate_names = take(&mut opts.crate_names);
    let opts = Arc::new(opts);

    // Initialize reqwest client
    let client = create_reqwest_client(opts.secure, opts.min_tls_version.map(|v| v.into()))?;

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

    let mut uithread = UIThread::new(!opts.no_confirm);

    let desired_targets = get_desired_targets(&opts.targets);

    // Compute install directory
    let install_path = get_install_path(opts.install_path.as_deref()).ok_or_else(|| {
        error!("No viable install path found of specified, try `--install-path`");
        miette!("No install path found or specified")
    })?;
    debug!("Using install path: {}", install_path.display());

    // Create a temporary directory for downloads etc.
    let temp_dir = TempDir::new()
        .map_err(BinstallError::from)
        .wrap_err("Creating a temporary directory failed.")?;

    let tasks: Vec<_> = crate_names
        .into_iter()
        .map(|crate_name| {
            tokio::spawn(resolve(
                opts.clone(),
                crate_name,
                desired_targets.clone(),
                cli_overrides.clone(),
                temp_dir.path().to_path_buf(),
                install_path.clone(),
                client.clone(),
            ))
        })
        .collect();

    let mut resolutions = Vec::with_capacity(tasks.len());
    for task in tasks {
        resolutions.push(await_task(task).await??);
    }

    for resolution in &resolutions {
        match resolution {
            Resolution::Fetch {
                fetcher, bin_files, ..
            } => {
                let fetcher_target = fetcher.target();
                // Prompt user for confirmation
                debug!(
                    "Found a binary install source: {} ({fetcher_target})",
                    fetcher.source_name()
                );

                if fetcher.is_third_party() {
                    warn!(
                        "The package will be downloaded from third-party source {}",
                        fetcher.source_name()
                    );
                } else {
                    info!(
                        "The package will be downloaded from {}",
                        fetcher.source_name()
                    );
                }

                info!("This will install the following binaries:");
                for file in bin_files {
                    info!("  - {}", file.preview_bin());
                }

                if !opts.no_symlinks {
                    info!("And create (or update) the following symlinks:");
                    for file in bin_files {
                        info!("  - {}", file.preview_link());
                    }
                }
            }
            Resolution::InstallFromSource { .. } => {
                warn!("The package will be installed from source (with cargo)",)
            }
        }
    }

    if !opts.dry_run {
        uithread.confirm().await?;
    }

    let desired_targets = desired_targets.get().await;
    let target = desired_targets
        .first()
        .ok_or_else(|| miette!("No viable targets found, try with `--targets`"))?;

    let tasks: Vec<_> = resolutions
        .into_iter()
        .map(|resolution| match resolution {
            Resolution::Fetch {
                fetcher,
                package,
                crate_name,
                version,
                bin_path,
                bin_files,
            } => tokio::spawn(install_from_package(
                fetcher,
                opts.clone(),
                package,
                crate_name,
                temp_dir.path().to_path_buf(),
                version,
                bin_path,
                bin_files,
            )),
            Resolution::InstallFromSource { package } => {
                if !opts.dry_run {
                    tokio::spawn(install_from_source(package, target.clone()))
                } else {
                    info!(
                        "Dry-run: running `cargo install {} --version {} --target {target}`",
                        package.name, package.version
                    );
                    tokio::spawn(async { Ok(()) })
                }
            }
        })
        .collect();

    for task in tasks {
        await_task(task).await??;
    }

    if !opts.no_cleanup {
        temp_dir.close().unwrap_or_else(|err| {
            warn!("Failed to clean up some resources: {err}");
        });
    }

    Ok(())
}

enum Resolution {
    Fetch {
        fetcher: Arc<dyn Fetcher>,
        package: Package<Meta>,
        crate_name: CrateName,
        version: String,
        bin_path: PathBuf,
        bin_files: Vec<bins::BinFile>,
    },
    InstallFromSource {
        package: Package<Meta>,
    },
}

async fn resolve(
    opts: Arc<Options>,
    crate_name: CrateName,
    desired_targets: DesiredTargets,
    cli_overrides: Arc<PkgOverride>,
    temp_dir: PathBuf,
    install_path: PathBuf,
    client: Client,
) -> Result<Resolution> {
    info!("Installing package: '{}'", crate_name);

    let mut version = match (&crate_name.version, &opts.version) {
        (Some(version), None) => version.to_string(),
        (None, Some(version)) => version.to_string(),
        (Some(_), Some(_)) => Err(BinstallError::DuplicateVersionReq)?,
        (None, None) => "*".to_string(),
    };

    if version
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
    {
        version.insert(0, '=');
    }

    // Fetch crate via crates.io, git, or use a local manifest path
    // TODO: work out which of these to do based on `opts.name`
    // TODO: support git-based fetches (whole repo name rather than just crate name)
    let manifest = match opts.manifest_path.clone() {
        Some(manifest_path) => load_manifest_path(manifest_path.join("Cargo.toml"))?,
        None => fetch_crate_cratesio(&client, &crate_name.name, &version).await?,
    };

    let package = manifest.package.unwrap();

    let (mut meta, binaries) = (
        package
            .metadata
            .as_ref()
            .and_then(|m| m.binstall.clone())
            .unwrap_or_default(),
        manifest.bin,
    );

    let mut fetchers = MultiFetcher::default();

    let desired_targets = desired_targets.get().await;

    for target in desired_targets {
        debug!("Building metadata for target: {target}");
        let mut target_meta = meta.clone();

        // Merge any overrides
        if let Some(o) = target_meta.overrides.get(target).cloned() {
            target_meta.merge(&o);
        }

        target_meta.merge(&cli_overrides);
        debug!("Found metadata: {target_meta:?}");

        let fetcher_data = Data {
            name: package.name.clone(),
            target: target.clone(),
            version: package.version.clone(),
            repo: package.repository.clone(),
            meta: target_meta,
        };

        fetchers.add(GhCrateMeta::new(&client, &fetcher_data).await);
        fetchers.add(QuickInstall::new(&client, &fetcher_data).await);
    }

    match fetchers.first_available().await {
        Some(fetcher) => {
            // Build final metadata
            let fetcher_target = fetcher.target();
            if let Some(o) = meta.overrides.get(&fetcher_target.to_owned()).cloned() {
                meta.merge(&o);
            }
            meta.merge(&cli_overrides);

            // Generate temporary binary path
            let bin_path = temp_dir.join(format!("bin-{}", crate_name.name));
            debug!("Using temporary binary path: {}", bin_path.display());

            let bin_files = collect_bin_files(
                fetcher.as_ref(),
                &package,
                meta,
                binaries,
                bin_path.clone(),
                install_path,
            )?;

            Ok(Resolution::Fetch {
                fetcher,
                package,
                crate_name,
                version,
                bin_path,
                bin_files,
            })
        }
        None => Ok(Resolution::InstallFromSource { package }),
    }
}

fn collect_bin_files(
    fetcher: &dyn Fetcher,
    package: &Package<Meta>,
    mut meta: PkgMeta,
    binaries: Vec<Product>,
    bin_path: PathBuf,
    install_path: PathBuf,
) -> Result<Vec<bins::BinFile>> {
    // Update meta
    if fetcher.source_name() == "QuickInstall" {
        // TODO: less of a hack?
        meta.bin_dir = "{ bin }{ binary-ext }".to_string();
    }

    // Check binaries
    if binaries.is_empty() {
        error!("No binaries specified (or inferred from file system)");
        return Err(miette!(
            "No binaries specified (or inferred from file system)"
        ));
    }

    // List files to be installed
    // based on those found via Cargo.toml
    let bin_data = bins::Data {
        name: package.name.clone(),
        target: fetcher.target().to_string(),
        version: package.version.clone(),
        repo: package.repository.clone(),
        meta,
        bin_path,
        install_path,
    };

    // Create bin_files
    let bin_files = binaries
        .iter()
        .map(|p| bins::BinFile::from_product(&bin_data, p))
        .collect::<Result<Vec<_>, BinstallError>>()?;

    Ok(bin_files)
}

#[allow(unused, clippy::too_many_arguments)]
async fn install_from_package(
    fetcher: Arc<dyn Fetcher>,
    opts: Arc<Options>,
    package: Package<Meta>,
    crate_name: CrateName,
    temp_dir: PathBuf,
    version: String,
    bin_path: PathBuf,
    bin_files: Vec<bins::BinFile>,
) -> Result<()> {
    // Download package
    if opts.dry_run {
        info!("Dry run, not downloading package");
    } else {
        fetcher.fetch_and_extract(&bin_path).await?;
    }

    #[cfg(incomplete)]
    {
        // Fetch and check package signature if available
        if let Some(pub_key) = meta.as_ref().map(|m| m.pub_key.clone()).flatten() {
            debug!("Found public key: {pub_key}");

            // Generate signature file URL
            let mut sig_ctx = ctx.clone();
            sig_ctx.format = "sig".to_string();
            let sig_url = sig_ctx.render(&pkg_url)?;

            debug!("Fetching signature file: {sig_url}");

            // Download signature file
            let sig_path = temp_dir.join(format!("{pkg_name}.sig"));
            download(&sig_url, &sig_path).await?;

            // TODO: do the signature check
            unimplemented!()
        } else {
            warn!("No public key found, package signature could not be validated");
        }
    }

    if opts.dry_run {
        info!("Dry run, not proceeding");
        return Ok(());
    }

    let cvs = metafiles::CrateVersionSource {
        name: crate_name.name.clone(),
        version: package.version.parse().into_diagnostic()?,
        source: metafiles::Source::Registry(
            url::Url::parse("https://github.com/rust-lang/crates.io-index").unwrap(),
        ),
    };

    info!("Installing binaries...");
    block_in_place(|| {
        for file in &bin_files {
            file.install_bin()?;
        }

        // Generate symlinks
        if !opts.no_symlinks {
            for file in &bin_files {
                file.install_link()?;
            }
        }

        let bins: BTreeSet<String> = bin_files.iter().map(|bin| bin.base_name.clone()).collect();

        {
            debug!("Writing .crates.toml");
            let mut c1 = metafiles::v1::CratesToml::load().unwrap_or_default();
            c1.insert(cvs.clone(), bins.clone());
            c1.write()?;
        }

        {
            debug!("Writing .crates2.json");
            let mut c2 = metafiles::v2::Crates2Json::load().unwrap_or_default();
            c2.insert(
                cvs.clone(),
                metafiles::v2::CrateInfo {
                    version_req: Some(version),
                    bins,
                    profile: "release".into(),
                    target: fetcher.target().to_string(),
                    rustc: format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
                    ..Default::default()
                },
            );
            c2.write()?;
        }

        Ok(())
    })
}

async fn install_from_source(package: Package<Meta>, target: String) -> Result<()> {
    debug!(
        "Running `cargo install {} --version {} --target {target}`",
        package.name, package.version
    );
    let mut child = Command::new("cargo")
        .arg("install")
        .arg(package.name)
        .arg("--version")
        .arg(package.version)
        .arg("--target")
        .arg(target)
        .spawn()
        .into_diagnostic()
        .wrap_err("Spawning cargo install failed.")?;
    debug!("Spawned command pid={:?}", child.id());

    let status = child
        .wait()
        .await
        .into_diagnostic()
        .wrap_err("Running cargo install failed.")?;
    if status.success() {
        info!("Cargo finished successfully");
        Ok(())
    } else {
        error!("Cargo errored! {status:?}");
        Err(miette!("Cargo install error"))
    }
}
