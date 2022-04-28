use std::path::PathBuf;

use log::{debug, error, info, warn, LevelFilter};
use simplelog::{ColorChoice, ConfigBuilder, TermLogger, TerminalMode};

use structopt::StructOpt;

use tempdir::TempDir;

use cargo_binstall::{
    bins,
    fetchers::{Data, Fetcher, GhCrateMeta, MultiFetcher, QuickInstall},
    *,
};

#[derive(Debug, StructOpt)]
struct Options {
    /// Package name or URL for installation
    /// This must be either a crates.io package name or github or gitlab url
    #[structopt()]
    name: String,

    /// Filter for package version to install
    #[structopt(long, default_value = "*")]
    version: String,

    /// Override binary target, ignoring compiled version
    #[structopt(long, default_value = TARGET)]
    target: String,

    /// Override install path for downloaded binary.
    /// Defaults to `$HOME/.cargo/bin`
    #[structopt(long)]
    install_path: Option<String>,

    /// Disable symlinking / versioned updates
    #[structopt(long)]
    no_symlinks: bool,

    /// Dry run, fetch and show changes without installing binaries
    #[structopt(long)]
    dry_run: bool,

    /// Disable interactive mode / confirmation
    #[structopt(long)]
    no_confirm: bool,

    /// Do not cleanup temporary files on success
    #[structopt(long)]
    no_cleanup: bool,

    /// Override manifest source.
    /// This skips searching crates.io for a manifest and uses
    /// the specified path directly, useful for debugging and
    /// when adding `binstall` support.
    #[structopt(long)]
    manifest_path: Option<PathBuf>,

    /// Utility log level
    #[structopt(long, default_value = "info")]
    log_level: LevelFilter,
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    // Filter extraneous arg when invoked by cargo
    // `cargo run -- --help` gives ["target/debug/cargo-binstall", "--help"]
    // `cargo binstall --help` gives ["/home/ryan/.cargo/bin/cargo-binstall", "binstall", "--help"]
    let mut args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "binstall" {
        args.remove(1);
    }

    // Load options
    let opts = Options::from_iter(args.iter());

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
    let temp_dir = TempDir::new("cargo-binstall")?;

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

    let (mut meta, binaries) = (
        package
            .metadata
            .map(|m| m.binstall)
            .flatten()
            .unwrap_or(PkgMeta::default()),
        manifest.bin,
    );

    // Merge any overrides
    if let Some(o) = meta.overrides.remove(&opts.target) {
        meta.merge(&o);
    }

    debug!("Found metadata: {:?}", meta);

    // Compute install directory
    let install_path = get_install_path(opts.install_path.as_deref()).ok_or_else(|| {
        error!("No viable install path found of specified, try `--install-path`");
        anyhow::anyhow!("No install path found or specified")
    })?;
    debug!("Using install path: {}", install_path.display());

    // Compute temporary directory for downloads
    let pkg_path = temp_dir
        .path()
        .join(format!("pkg-{}.{}", opts.name, meta.pkg_fmt));
    debug!("Using temporary download path: {}", pkg_path.display());

    let fetcher_data = Data {
        name: package.name.clone(),
        target: opts.target.clone(),
        version: package.version.clone(),
        repo: package.repository.clone(),
        meta: meta.clone(),
    };

    // Try github releases, then quickinstall
    let mut fetchers = MultiFetcher::default();
    fetchers.add(GhCrateMeta::new(&fetcher_data).await);
    fetchers.add(QuickInstall::new(&fetcher_data).await);

    let fetcher = fetchers.first_available().await.ok_or_else(|| {
        error!("File does not exist remotely, cannot proceed");
        anyhow::anyhow!("No viable remote package found")
    })?;

    // Prompt user for third-party source
    if fetcher.is_third_party() {
        warn!(
            "The package will be downloaded from third-party source {}",
            fetcher.source_name()
        );
        if !opts.no_confirm && !opts.dry_run && !confirm()? {
            warn!("Installation cancelled");
            return Ok(());
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

        // Bypass cleanup if disabled
        if opts.no_cleanup {
            let _ = temp_dir.into_path();
        }

        if binaries.is_empty() {
            error!("No binaries specified (or inferred from file system)");
            return Err(anyhow::anyhow!(
                "No binaries specified (or inferred from file system)"
            ));
        }
    }

    // List files to be installed
    // based on those found via Cargo.toml
    let bin_data = bins::Data {
        name: package.name.clone(),
        target: opts.target.clone(),
        version: package.version.clone(),
        repo: package.repository.clone(),
        meta,
        bin_path,
        install_path,
    };

    let bin_files = binaries
        .iter()
        .map(|p| bins::BinFile::from_product(&bin_data, p))
        .collect::<Result<Vec<_>, anyhow::Error>>()?;

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

    if !opts.no_confirm && !confirm()? {
        warn!("Installation cancelled");
        return Ok(());
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

    info!("Installation complete!");
    Ok(())
}
