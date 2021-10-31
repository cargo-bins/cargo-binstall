use std::path::{PathBuf};

use log::{debug, info, warn, error, LevelFilter};
use simplelog::{TermLogger, ConfigBuilder, TerminalMode};

use structopt::StructOpt;

use tempdir::TempDir;

use cargo_binstall::*;


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

    /// Override Cargo.toml package manifest pkg-url.
    #[structopt(long)]
    pkg_url: Option<String>,

    /// Override Cargo.toml package manifest pkg-fmt.
    #[structopt(long)]
    pkg_fmt: Option<PkgFmt>,

    /// Override Cargo.toml package manifest bin-dir.
    #[structopt(long)]
    bin_path: Option<String>,
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
    let cli_overrides = PkgOverride {
        pkg_url: opts.pkg_url.clone(),
        pkg_fmt: opts.pkg_fmt,
        bin_dir: opts.bin_dir.clone(),
    };

    // Setup logging
    let mut log_config = ConfigBuilder::new();
    log_config.add_filter_ignore("hyper".to_string());
    log_config.add_filter_ignore("reqwest".to_string());
    log_config.set_location_level(LevelFilter::Off);
    TermLogger::init(opts.log_level, log_config.build(), TerminalMode::Mixed).unwrap();

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
        package.metadata.map(|m| m.binstall ).flatten().unwrap_or_default(),
        manifest.bin,
    );

    // Merge any overrides
    if let Some(o) = meta.overrides.remove(&opts.target) {
        meta.merge(&o);
    }
    meta.merge(&cli_overrides);

    // Generate context for URL interpolation
    let ctx = Context { 
        name: opts.name.clone(), 
        repo: package.repository, 
        target: opts.target.clone(), 
        version: package.version.clone(),
        format: meta.pkg_fmt.to_string(),
        bin: None,
    };

    debug!("Using context: {:?}", ctx);
    
    // Interpolate version / target / etc.
    let rendered = ctx.render(&meta.pkg_url)?;

    // Compute install directory
    let install_path = match get_install_path(opts.install_path.as_deref()) {
        Some(p) => p,
        None => {
            error!("No viable install path found of specified, try `--install-path`");
            return Err(anyhow::anyhow!("No install path found or specified"));
        }
    };

    debug!("Using install path: {}", install_path.display());

    info!("Downloading package from: '{}'", rendered);

    // Download package
    let pkg_path = temp_dir.path().join(format!("pkg-{}.{}", opts.name, meta.pkg_fmt));
    download(&rendered, pkg_path.to_str().unwrap()).await?;

    #[cfg(incomplete)]
    {
        // Fetch and check package signature if available
        if let Some(pub_key) = meta.as_ref().map(|m| m.pub_key.clone() ).flatten() {
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

    // Extract files
    let bin_path = temp_dir.path().join(format!("bin-{}", opts.name));
    extract(&pkg_path, meta.pkg_fmt, &bin_path)?;

    // Bypass cleanup if disabled
    if opts.no_cleanup {
        let _ = temp_dir.into_path();
    }

    if binaries.is_empty() {
        error!("No binaries specified (or inferred from file system)");
        return Err(anyhow::anyhow!("No binaries specified (or inferred from file system)"));
    }

    // List files to be installed
    // based on those found via Cargo.toml
    let bin_files = binaries.iter().map(|p| {
        // Fetch binary base name
        let base_name = p.name.clone().unwrap();

        // Generate binary path via interpolation
        let mut bin_ctx = ctx.clone();
        bin_ctx.bin = Some(base_name.clone());
        
        // Append .exe to windows binaries
        bin_ctx.format = match &opts.target.clone().contains("windows") {
            true => ".exe".to_string(),
            false => "".to_string(),
        };

        // Generate install paths
        // Source path is the download dir + the generated binary path
        let source_file_path = bin_ctx.render(&meta.bin_dir)?;
        let source = if meta.pkg_fmt == PkgFmt::Bin {
            bin_path.clone()
        } else {
            bin_path.join(&source_file_path)
        };

        // Destination path is the install dir + base-name-version{.format}
        let dest_file_path = bin_ctx.render("{ bin }-v{ version }{ format }")?; 
        let dest = install_path.join(dest_file_path);

        // Link at install dir + base name
        let link = install_path.join(&base_name);

        Ok((base_name, source, dest, link))
    }).collect::<Result<Vec<_>, anyhow::Error>>()?;

    // Prompt user for confirmation
    info!("This will install the following binaries:");
    for (name, source, dest, _link) in &bin_files {
        info!("  - {} ({} -> {})", name, source.file_name().unwrap().to_string_lossy(), dest.display());
    }

    if !opts.no_symlinks {
        info!("And create (or update) the following symlinks:");
        for (name, _source, dest, link) in &bin_files {
            info!("  - {} ({} -> {})", name, dest.display(), link.display());
        }
    }

    if !opts.no_confirm && !confirm()? {
        warn!("Installation cancelled");
        return Ok(())
    }

    info!("Installing binaries...");

    // Install binaries
    for (_name, source, dest, _link) in &bin_files {
        // TODO: check if file already exists
        std::fs::copy(source, dest)?;

        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    // Generate symlinks
    if !opts.no_symlinks {
        for (_name, _source, dest, link) in &bin_files {
            // Remove existing symlink
            // TODO: check if existing symlink is correct
            if link.exists() {
                std::fs::remove_file(&link)?;
            }

            #[cfg(target_family = "unix")]
            std::os::unix::fs::symlink(dest, link)?;
            #[cfg(target_family = "windows")]
            std::os::windows::fs::symlink_file(dest, link)?;
        }
    }
    
    info!("Installation complete!");

    Ok(())
}

