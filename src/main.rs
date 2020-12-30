use std::path::{PathBuf};

use log::{debug, info, error, LevelFilter};
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

    /// Override format for binary file download.
    /// Defaults to `tgz`
    #[structopt(long)]
    pkg_fmt: Option<PkgFmt>,

    /// Override install path for downloaded binary.
    /// Defaults to `$HOME/.cargo/bin`
    #[structopt(long)]
    install_path: Option<String>,

    #[structopt(flatten)]
    overrides: Overrides,

    /// Do not cleanup temporary files on success
    #[structopt(long)]
    no_cleanup: bool,

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
    let pkg_fmt = match (opts.pkg_fmt, meta.as_ref().map(|m| m.pkg_fmt.clone() ).flatten()) {
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
        format: pkg_fmt.clone(),
    };

    debug!("Using context: {:?}", ctx);
    
    // Interpolate version / target / etc.
    let rendered = ctx.render(&pkg_url)?;

    info!("Downloading package from: '{}'", rendered);

    // Download package
    let pkg_path = temp_dir.path().join(format!("pkg-{}.{}", pkg_name, pkg_fmt));
    download(&rendered, pkg_path.to_str().unwrap()).await?;


    if opts.no_cleanup {
        // Do not delete temporary directory
        let _ = temp_dir.into_path();
    }

    // TODO: check signature

    // Compute install directory
    let install_path = match get_install_path(opts.install_path) {
        Some(p) => p,
        None => {
            error!("No viable install path found of specified, try `--install-path`");
            return Err(anyhow::anyhow!("No install path found or specified"));
        }
    };

    // Install package
    info!("Installing to: '{}'", install_path);
    extract(&pkg_path, pkg_fmt, &install_path)?;

    
    info!("Installation done!");

    Ok(())
}



/// Fetch install path
/// roughly follows https://doc.rust-lang.org/cargo/commands/cargo-install.html#description
fn get_install_path(opt: Option<String>) -> Option<String> {
    // Command line override first first
    if let Some(p) = opt {
        return Some(p)
    }

    // Environmental variables
    if let Ok(p) = std::env::var("CARGO_INSTALL_ROOT") {
        return Some(format!("{}/bin", p))
    }
    if let Ok(p) = std::env::var("CARGO_HOME") {
        return Some(format!("{}/bin", p))
    }

    // Standard $HOME/.cargo/bin
    if let Some(mut d) = dirs::home_dir() {
        d.push(".cargo/bin");
        let p = d.as_path();

        if p.exists() {
            return Some(p.to_str().unwrap().to_owned());
        }
    }

    // Local executable dir if no cargo is found
    if let Some(d) = dirs::executable_dir() {
        return Some(d.to_str().unwrap().to_owned());
    }

    None
}