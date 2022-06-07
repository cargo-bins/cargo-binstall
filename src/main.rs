use std::{
    ffi::OsString,
    path::PathBuf,
    process::{ExitCode, Termination},
    str::FromStr,
    time::{Duration, Instant},
};

use cargo_toml::{Package, Product};
use clap::Parser;
use log::{debug, error, info, warn, LevelFilter};
use miette::{miette, IntoDiagnostic, Result, WrapErr};
use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};
use tempfile::TempDir;
use tokio::{process::Command, runtime::Runtime, task::JoinError};

use cargo_binstall::{
    bins,
    fetchers::{Data, Fetcher, GhCrateMeta, MultiFetcher, QuickInstall},
    *,
};

#[derive(Debug, Parser)]
#[clap(version, about = "Install a Rust binary... from binaries!")]
struct Options {
    /// Package name for installation.
    ///
    /// This must be a crates.io package name.
    #[clap(value_name = "crate")]
    name: String,

    /// Semver filter to select the package version to install.
    ///
    /// This is in Cargo.toml dependencies format: `--version 1.2.3` is equivalent to
    /// `--version "^1.2.3"`. Use `=1.2.3` to install a specific version.
    #[clap(long, default_value = "*")]
    version: String,

    /// Override binary target set. Defaults to a set of targets based on the current platform.
    #[clap(help_heading = "OVERRIDES", long)]
    target: Option<String>,

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
    #[clap(long, default_value = "info")]
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
    let mut opts = Options::parse_from(args.iter());
    let cli_overrides = PkgOverride {
        pkg_url: opts.pkg_url.take(),
        pkg_fmt: opts.pkg_fmt.take(),
        bin_dir: opts.bin_dir.take(),
    };

    // Setup logging
    let mut log_config = ConfigBuilder::new();
    log_config.add_filter_ignore("hyper".to_string());
    log_config.add_filter_ignore("reqwest".to_string());
    log_config.set_location_level(LevelFilter::Off);
    TermLogger::init(
        opts.log_level,
        log_config.build(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    )
    .unwrap();

    // Create a temporary directory for downloads etc.
    let temp_dir = TempDir::new()
        .map_err(BinstallError::from)
        .wrap_err("Creating a temporary directory failed.")?;

    info!("Installing package: '{}'", opts.name);

    // Fetch crate via crates.io, git, or use a local manifest path
    // TODO: work out which of these to do based on `opts.name`
    // TODO: support git-based fetches (whole repo name rather than just crate name)
    let manifest_path = match opts.manifest_path.clone() {
        Some(p) => p,
        None => fetch_crate_cratesio(&opts.name, &opts.version, temp_dir.path()).await?,
    };

    debug!("Reading manifest: {}", manifest_path.display());
    let manifest = load_manifest_path(manifest_path.join("Cargo.toml"))?;
    let package = manifest.package.unwrap();

    let is_plain_version = semver::Version::from_str(&opts.version).is_ok();
    if is_plain_version && package.version != opts.version {
        warn!("Warning!");
        eprintln!(
            "{:?}",
            miette::Report::new(BinstallError::VersionWarning {
                ver: package.version.clone(),
                req: opts.version.clone()
            })
        );

        if !opts.no_confirm && !opts.dry_run {
            confirm()?;
        }
    }

    let (mut meta, binaries) = (
        package
            .metadata
            .as_ref()
            .and_then(|m| m.binstall.clone())
            .unwrap_or_default(),
        manifest.bin,
    );

    // Merge any overrides
    if let Some(o) = meta.overrides.remove(&opts.target) {
        meta.merge(&o);
    }

    meta.merge(&cli_overrides);
    debug!("Found metadata: {:?}", meta);

    // Compute install directory
    let install_path = get_install_path(opts.install_path.as_deref()).ok_or_else(|| {
        error!("No viable install path found of specified, try `--install-path`");
        miette!("No install path found or specified")
    })?;
    debug!("Using install path: {}", install_path.display());

    // Compute temporary directory for downloads
    let pkg_path = temp_dir
        .path()
        .join(format!("pkg-{}.{}", opts.name, meta.pkg_fmt));
    debug!("Using temporary download path: {}", pkg_path.display());

    let fetcher_data: Vec<_> = detect_targets()
        .await
        .into_iter()
        .map(|target| Data {
            name: package.name.clone(),
            target: target.into(),
            version: package.version.clone(),
            repo: package.repository.clone(),
            meta: meta.clone(),
        })
        .collect();

    // Try github releases, then quickinstall
    let mut fetchers = MultiFetcher::default();
    for data in &fetcher_data {
        fetchers.add(GhCrateMeta::new(data).await);
        fetchers.add(QuickInstall::new(data).await);
    }

    match fetchers.first_available().await {
        Some(fetcher) => {
            install_from_package(
                binaries,
                &*fetcher,
                install_path,
                meta,
                opts,
                package,
                pkg_path,
                temp_dir,
            )
            .await
        }
        None => {
            temp_dir.close().unwrap_or_else(|err| {
                warn!("Failed to clean up some resources: {err}");
            });
            install_from_source(opts, package).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn install_from_package(
    binaries: Vec<Product>,
    fetcher: &dyn Fetcher,
    install_path: PathBuf,
    mut meta: PkgMeta,
    opts: Options,
    package: Package<Meta>,
    pkg_path: PathBuf,
    temp_dir: TempDir,
) -> Result<()> {
    // Prompt user for third-party source
    if fetcher.is_third_party() {
        warn!(
            "The package will be downloaded from third-party source {}",
            fetcher.source_name()
        );
        if !opts.no_confirm && !opts.dry_run {
            confirm()?;
        }
    } else {
        info!(
            "The package will be downloaded from {}",
            fetcher.source_name()
        );
    }

    if fetcher.source_name() == "QuickInstall" {
        // TODO: less of a hack?
        meta.bin_dir = "{ bin }{ binary-ext }".to_string();
    }

    // Download package
    if opts.dry_run {
        info!("Dry run, not downloading package");
    } else {
        fetcher.fetch(&pkg_path).await?;
    }

    #[cfg(incomplete)]
    {
        // Fetch and check package signature if available
        if let Some(pub_key) = meta.as_ref().map(|m| m.pub_key.clone()).flatten() {
            debug!("Found public key: {}", pub_key);

            // Generate signature file URL
            let mut sig_ctx = ctx.clone();
            sig_ctx.format = "sig".to_string();
            let sig_url = sig_ctx.render(&pkg_url)?;

            debug!("Fetching signature file: {}", sig_url);

            // Download signature file
            let sig_path = temp_dir.path().join(format!("{}.sig", pkg_name));
            download(&sig_url, &sig_path).await?;

            // TODO: do the signature check
            unimplemented!()
        } else {
            warn!("No public key found, package signature could not be validated");
        }
    }

    let bin_path = temp_dir.path().join(format!("bin-{}", opts.name));
    debug!("Using temporary binary path: {}", bin_path.display());

    if !opts.dry_run {
        // Extract files
        extract(&pkg_path, fetcher.pkg_fmt(), &bin_path)?;

        if binaries.is_empty() {
            error!("No binaries specified (or inferred from file system)");
            return Err(miette!(
                "No binaries specified (or inferred from file system)"
            ));
        }
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

    let bin_files = binaries
        .iter()
        .map(|p| bins::BinFile::from_product(&bin_data, p))
        .collect::<Result<Vec<_>, BinstallError>>()?;

    // Prompt user for confirmation
    info!("This will install the following binaries:");
    for file in &bin_files {
        info!("  - {}", file.preview_bin());
    }

    if !opts.no_symlinks {
        info!("And create (or update) the following symlinks:");
        for file in &bin_files {
            info!("  - {}", file.preview_link());
        }
    }

    if opts.dry_run {
        info!("Dry run, not proceeding");
        return Ok(());
    }

    if !opts.no_confirm {
        confirm()?;
    }

    info!("Installing binaries...");
    for file in &bin_files {
        file.install_bin()?;
    }

    // Generate symlinks
    if !opts.no_symlinks {
        for file in &bin_files {
            file.install_link()?;
        }
    }

    if opts.no_cleanup {
        let _ = temp_dir.into_path();
    } else {
        temp_dir.close().unwrap_or_else(|err| {
            warn!("Failed to clean up some resources: {err}");
        });
    }

    Ok(())
}

async fn install_from_source(opts: Options, package: Package<Meta>) -> Result<()> {
    // Prompt user for source install
    warn!("The package will be installed from source (with cargo)",);
    if !opts.no_confirm && !opts.dry_run {
        confirm()?;
    }

    if opts.dry_run {
        info!(
            "Dry-run: running `cargo install {} --version {} --target {}`",
            package.name, package.version, opts.target
        );
        Ok(())
    } else {
        debug!(
            "Running `cargo install {} --version {} --target {}`",
            package.name, package.version, opts.target
        );
        let mut child = Command::new("cargo")
            .arg("install")
            .arg(package.name)
            .arg("--version")
            .arg(package.version)
            .arg("--target")
            .arg(opts.target)
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
            error!("Cargo errored! {:?}", status);
            Err(miette!("Cargo install error"))
        }
    }
}
