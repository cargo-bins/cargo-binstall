use std::path::{PathBuf};

use log::{debug, info, warn, error, LevelFilter};
use simplelog::{TermLogger, ConfigBuilder, TerminalMode};

use structopt::StructOpt;

use cargo_toml::Manifest;

use tempdir::TempDir;

use cargo_binstall::*;


#[derive(Debug, StructOpt)]
struct Options {
    /// Package name or URL for installation
    /// This must be either a crates.io package name or github or gitlab url
    #[structopt()]
    name: String,

    /// Package version to instal
    #[structopt(long)]
    version: Option<String>,

    /// Override binary target, ignoring compiled version
    #[structopt(long, default_value = TARGET)]
    target: String,

    /// Override install path for downloaded binary.
    /// Defaults to `$HOME/.cargo/bin`
    #[structopt(long)]
    install_path: Option<String>,

    #[structopt(flatten)]
    overrides: Overrides,

    /// Do not cleanup temporary files on success
    #[structopt(long)]
    no_cleanup: bool,

    /// Disable interactive mode / confirmation
    #[structopt(long)]
    no_confirm: bool,

    /// Disable symlinking / versioned updates
    #[structopt(long)]
    no_symlinks: bool,

    /// Utility log level
    #[structopt(long, default_value = "info")]
    log_level: LevelFilter,
}

#[derive(Debug, StructOpt)]
pub struct Overrides {

    /// Override the package name. 
    /// This is only useful for diagnostics when using the default `pkg_url`
    /// as you can otherwise customise this in the path.
    /// Defaults to the crate name.
    #[structopt(long)]
    pkg_name: Option<String>,

    /// Override the package path template.
    /// If no `metadata.pkg_url` key is set or `--pkg-url` argument provided, this
    /// defaults to `{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.tgz`
    #[structopt(long)]
    pkg_url: Option<String>,

    /// Override format for binary file download.
    /// Defaults to `tgz`
    #[structopt(long)]
    pkg_fmt: Option<PkgFmt>,

    /// Override manifest source.
    /// This skips searching crates.io for a manifest and uses
    /// the specified path directly, useful for debugging
    #[structopt(long)]
    manifest_path: Option<PathBuf>,
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
    TermLogger::init(opts.log_level, log_config.build(), TerminalMode::Mixed).unwrap();

    // Create a temporary directory for downloads etc.
    let temp_dir = TempDir::new("cargo-binstall")?;

    // Fetch crate via crates.io, git, or use a local manifest path
    // TODO: work out which of these to do based on `opts.name`
    let crate_path = match opts.overrides.manifest_path {
        Some(p) => p,
        None => fetch_crate_cratesio(&opts.name, opts.version.as_deref(), temp_dir.path()).await?,
    };

    // Read cargo manifest
    let manifest_path = crate_path.join("Cargo.toml");

    debug!("Reading manifest: {}", manifest_path.to_str().unwrap());
    let package = match Manifest::<Meta>::from_path_with_metadata(&manifest_path) {
        Ok(m) => m.package.unwrap(),
        Err(e) => {
            error!("Error reading manifest '{}': {:?}", manifest_path.to_str().unwrap(), e);
            return Err(e.into());
        },
    };

    let meta = package.metadata;
    debug!("Retrieved metadata: {:?}", meta);

    // Select which package path to use
    let pkg_url = match (opts.overrides.pkg_url, meta.as_ref().map(|m| m.pkg_url.clone() ).flatten()) {
        (Some(p), _) => {
            info!("Using package url override: '{}'", p);
            p
        },
        (_, Some(m)) => {
            info!("Using package url: '{}'", &m);
            m
        },
        _ => {
            info!("No `pkg-url` key found in Cargo.toml or `--pkg-url` argument provided");
            info!("Using default url: {}", DEFAULT_PKG_PATH);
            DEFAULT_PKG_PATH.to_string()
        },
    };

    // Select bin format to use
    let pkg_fmt = match (opts.overrides.pkg_fmt, meta.as_ref().map(|m| m.pkg_fmt.clone() ).flatten()) {
        (Some(o), _) => o,
        (_, Some(m)) => m.clone(),
        _ => PkgFmt::Tgz,
    };

    // Override package name if required
    let pkg_name = match (&opts.overrides.pkg_name, meta.as_ref().map(|m| m.pkg_name.clone() ).flatten()) {
        (Some(o), _) => o.clone(),
        (_, Some(m)) => m,
        _ => opts.name.clone(),
    };

    // Generate context for interpolation
    let ctx = Context { 
        name: pkg_name.to_string(), 
        repo: package.repository, 
        target: opts.target.clone(), 
        version: package.version.clone(),
        format: pkg_fmt.to_string(),
    };

    debug!("Using context: {:?}", ctx);
    
    // Interpolate version / target / etc.
    let rendered = ctx.render(&pkg_url)?;

    // Compute install directory
    let install_path = match get_install_path(opts.install_path) {
        Some(p) => p,
        None => {
            error!("No viable install path found of specified, try `--install-path`");
            return Err(anyhow::anyhow!("No install path found or specified"));
        }
    };

    debug!("Using install path: {}", install_path.display());

    info!("Downloading package from: '{}'", rendered);

    // Download package
    let pkg_path = temp_dir.path().join(format!("pkg-{}.{}", pkg_name, pkg_fmt));
    download(&rendered, pkg_path.to_str().unwrap()).await?;

    // Fetch and check package signature if available
    if let Some(pub_key) = meta.as_ref().map(|m| m.pkg_pub_key.clone() ).flatten() {
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

    // Extract files
    let bin_path = temp_dir.path().join(format!("bin-{}", pkg_name));
    extract(&pkg_path, pkg_fmt, &bin_path)?;

    // Bypass cleanup if disabled
    if opts.no_cleanup {
        let _ = temp_dir.into_path();
    }

    // List files to be installed
    // TODO: check extracted files are sensible / filter by allowed files
    // TODO: this seems overcomplicated / should be able to be simplified?
    let bin_files = std::fs::read_dir(&bin_path)?;
    let bin_files: Vec<_> = bin_files.filter_map(|f| f.ok() ).map(|f| {
        let source = f.path().to_owned();
        let name = source.file_name().map(|v| v.to_str()).flatten().unwrap().to_string();

        // Trim target and version from name if included in binary file name
        let base_name = name.replace(&format!("-{}", ctx.target), "")
                .replace(&format!("-v{}", ctx.version), "")
                .replace(&format!("-{}", ctx.version), "");

        // Generate install destination with version suffix
        let dest = install_path.join(format!("{}-v{}", base_name, ctx.version));

        // Generate symlink path from base name
        let link = install_path.join(&base_name);

        (base_name, source, dest, link)
    }).collect();


    // Prompt user for confirmation
    info!("This will install the following files:");
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

    // Install binaries
    for (_name, source, dest, _link) in &bin_files {
        // TODO: check if file already exists
        std::fs::copy(source, dest)?;
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

