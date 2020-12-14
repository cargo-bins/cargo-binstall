use std::time::Duration;
use std::path::{PathBuf, Path};

use log::{debug, info, error, LevelFilter};
use simplelog::{TermLogger, ConfigBuilder, TerminalMode};

use structopt::StructOpt;
use serde::{Serialize, Deserialize};

use crates_io_api::AsyncClient;
use cargo_toml::Manifest;

use tempdir::TempDir;
use flate2::read::GzDecoder;
use tar::Archive;

use tinytemplate::TinyTemplate;

/// Compiled target triple, used as default for binary fetching
const TARGET: &'static str = env!("TARGET");

/// Default binary path for use if no path is specified
const DEFAULT_BIN_PATH: &'static str = "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ format }";

/// Binary format enumeration
#[derive(Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
#[derive(strum_macros::Display, strum_macros::EnumString, strum_macros::EnumVariantNames)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum PkgFmt {
    /// Download format is TAR (uncompressed)
    Tar,
    /// Download format is TGZ (TAR + GZip)
    Tgz,
    /// Download format is raw / binary
    Bin,
}

#[derive(Debug, StructOpt)]
struct Options {
    /// Crate name to install
    #[structopt()]
    name: String,

    /// Crate version to install
    #[structopt(long)]
    version: Option<String>,

    /// Override the package path template.
    /// If no `metadata.pkg_url` key is set or `--pkg-url` argument provided, this
    /// defaults to `{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.tgz`
    #[structopt(long)]
    pkg_url: Option<String>,

    /// Override format for binary file download.
    /// Defaults to `tgz`
    #[structopt(long)]
    pkg_fmt: Option<PkgFmt>,

    /// Override the package name. 
    /// This is only useful for diagnostics when using the default `pkg_url`
    /// as you can otherwise customise this in the path.
    /// Defaults to the crate name.
    #[structopt(long)]
    pkg_name: Option<String>,

    /// Override install path for downloaded binary.
    /// Defaults to `$HOME/.cargo/bin`
    #[structopt(long)]
    install_path: Option<String>,

    /// Override binary target, ignoring compiled version
    #[structopt(long, default_value = TARGET)]
    target: String,

    /// Override manifest source.
    /// This skips searching crates.io for a manifest and uses
    /// the specified path directly, useful for debugging
    #[structopt(long)]
    manifest_path: Option<PathBuf>,

    /// Utility log level
    #[structopt(long, default_value = "info")]
    log_level: LevelFilter,

    /// Do not cleanup temporary files on success
    #[structopt(long)]
    no_cleanup: bool,
}


/// Metadata for cargo-binstall exposed via cargo.toml
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Meta {
    /// Path template override for binary downloads
    pub pkg_url: Option<String>,
    /// Package name override for binary downloads
    pub pkg_name: Option<String>,
    /// Format override for binary downloads
    pub pkg_fmt: Option<PkgFmt>,
}

/// Template for constructing download paths
#[derive(Clone, Debug, Serialize)]
pub struct Context {
    name: String,
    repo: Option<String>,
    target: String,
    version: String,
    format: PkgFmt,
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
    let crate_path = match opts.manifest_path {
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

    // Select which binary path to use
    let pkg_url = match (opts.pkg_url, meta.as_ref().map(|m| m.pkg_url.clone() ).flatten()) {
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
            info!("Using default url: {}", DEFAULT_BIN_PATH);
            DEFAULT_BIN_PATH.to_string()
        },
    };

    // Select bin format to use
    let pkg_fmt = match (opts.pkg_fmt, meta.as_ref().map(|m| m.pkg_fmt.clone() ).flatten()) {
        (Some(o), _) => o,
        (_, Some(m)) => m.clone(),
        _ => PkgFmt::Tgz,
    };

    // Override package name if required
    let pkg_name = match (&opts.pkg_name, meta.as_ref().map(|m| m.pkg_name.clone() ).flatten()) {
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
    let mut tt = TinyTemplate::new();
    tt.add_template("path", &pkg_url)?;
    let rendered = tt.render("path", &ctx)?;

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

/// Download a file from the provided URL to the provided path
async fn download<P: AsRef<Path>>(url: &str, path: P) -> Result<(), anyhow::Error> {

    debug!("Downloading from: '{}'", url);

    let resp = reqwest::get(url).await?;

    if !resp.status().is_success() {
        error!("Download error: {}", resp.status());
        return Err(anyhow::anyhow!(resp.status()));
    }

    let bytes = resp.bytes().await?;

    debug!("Download OK, writing to file: '{:?}'", path.as_ref());

    std::fs::write(&path, bytes)?;

    Ok(())
}

fn extract<S: AsRef<Path>, P: AsRef<Path>>(source: S, fmt: PkgFmt, path: P) -> Result<(), anyhow::Error> {
    match fmt {
        PkgFmt::Tar => {
            // Extract to install dir
            debug!("Extracting from archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());

            let dat = std::fs::File::open(source)?;
            let mut tar = Archive::new(dat);

            tar.unpack(path)?;
        },
        PkgFmt::Tgz => {
            // Extract to install dir
            debug!("Decompressing from archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());

            let dat = std::fs::File::open(source)?;
            let tar = GzDecoder::new(dat);
            let mut tgz = Archive::new(tar);

            tgz.unpack(path)?;
        },
        PkgFmt::Bin => {
            debug!("Copying data from archive '{:?}' to `{:?}`", source.as_ref(), path.as_ref());
            // Copy to install dir
            std::fs::copy(source, path)?;
        },
    };

    Ok(())
}

/// Fetch a crate by name and version from crates.io
async fn fetch_crate_cratesio(name: &str, version: Option<&str>, temp_dir: &Path) -> Result<PathBuf, anyhow::Error> {
    // Build crates.io api client and fetch info
    // TODO: support git-based fetches (whole repo name rather than just crate name)
    let api_client = AsyncClient::new("cargo-binstall (https://github.com/ryankurte/cargo-binstall)", Duration::from_millis(100))?;

    info!("Fetching information for crate: '{}'", name);

    // Fetch overall crate info
    let info = match api_client.get_crate(name.as_ref()).await {
        Ok(i) => i,
        Err(e) => {
            error!("Error fetching information for crate {}: {}", name, e);
            return Err(e.into())
        }
    };

    // Use specified or latest version
    let version_num = match version {
        Some(v) => v.to_string(),
        None => info.crate_data.max_version,
    };

    // Fetch crates.io information for the specified version
    // TODO: could do a semver match and sort here?
    let version = match info.versions.iter().find(|v| v.num == version_num) {
        Some(v) => v,
        None => {
            error!("No crates.io information found for crate: '{}' version: '{}'", 
                    name, version_num);
            return Err(anyhow::anyhow!("No crate information found"));
        }
    };

    info!("Found information for crate version: '{}'", version.num);
    
    // Download crate to temporary dir (crates.io or git?)
    let crate_url = format!("https://crates.io/{}", version.dl_path);
    let tgz_path = temp_dir.join(format!("{}.tgz", name));

    debug!("Fetching crate from: {}", crate_url);    

    // Download crate
    download(&crate_url, &tgz_path).await?;

    // Decompress downloaded tgz
    debug!("Decompressing crate archive");
    extract(&tgz_path, PkgFmt::Tgz, &temp_dir)?;
    let crate_path = temp_dir.join(format!("{}-{}", name, version_num));

    // Return crate directory
    Ok(crate_path)
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