use std::{
    ffi::OsString,
    fmt,
    num::{NonZeroU64, ParseIntError},
    path::PathBuf,
    str::FromStr,
};

use binstalk::{
    errors::BinstallError,
    helpers::remote::tls::Version,
    manifests::cargo_toml_binstall::PkgFmt,
    ops::resolve::{CrateName, VersionReqExt},
};
use clap::{Parser, ValueEnum};
use log::LevelFilter;
use semver::VersionReq;

#[derive(Debug, Parser)]
#[clap(
    version,
    about = "Install a Rust binary... from binaries!",
    arg_required_else_help(true),
    // Avoid conflict with version_req
    disable_version_flag(true),
)]
pub struct Args {
    /// Packages to install.
    ///
    /// Syntax: crate[@version]
    ///
    /// Each value is either a crate name alone, or a crate name followed by @ and the version to
    /// install. The version syntax is as with the --version option.
    ///
    /// When multiple names are provided, the --version option and override option
    /// `manifest_path` is unavailable due to ambiguity.
    ///
    /// If duplicate names are provided, the last one (and their version requirement)
    /// is kept.
    #[clap(
    help_heading = "Package selection",
    value_name = "crate[@version]",
    required_unless_present_any = ["version", "help"],
)]
    pub crate_names: Vec<CrateName>,

    /// Package version to install.
    ///
    /// Takes either an exact semver version or a semver version requirement expression, which will
    /// be resolved to the highest matching version available.
    ///
    /// Cannot be used when multiple packages are installed at once, use the attached version
    /// syntax in that case.
    #[clap(
        help_heading = "Package selection",
        long = "version",
        value_parser(VersionReq::parse_from_cli)
    )]
    pub version_req: Option<VersionReq>,

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
    pub targets: Option<Vec<String>>,

    /// Override Cargo.toml package manifest path.
    ///
    /// This skips searching crates.io for a manifest and uses the specified path directly, useful
    /// for debugging and when adding Binstall support. This may be either the path to the folder
    /// containing a Cargo.toml file, or the Cargo.toml file itself.
    #[clap(help_heading = "Overrides", long)]
    pub manifest_path: Option<PathBuf>,

    /// Override Cargo.toml package manifest bin-dir.
    #[clap(help_heading = "Overrides", long)]
    pub bin_dir: Option<String>,

    /// Override Cargo.toml package manifest pkg-fmt.
    ///
    /// The available package formats are:
    ///
    /// - tar: download format is TAR (uncompressed)
    ///
    /// - tbz2: Download format is TAR + Bzip2
    ///
    /// - tgz: Download format is TGZ (TAR + GZip)
    ///
    /// - txz: Download format is TAR + XZ
    ///
    /// - tzstd: Download format is TAR + Zstd
    ///
    /// - zip: Download format is Zip
    ///
    /// - bin: Download format is raw / binary
    #[clap(help_heading = "Overrides", long, value_name = "PKG_FMT")]
    pub pkg_fmt: Option<PkgFmt>,

    /// Override Cargo.toml package manifest pkg-url.
    #[clap(help_heading = "Overrides", long)]
    pub pkg_url: Option<String>,

    /// Override the rate limit duration.
    ///
    /// By default, cargo-binstall allows one request per 5 ms.
    ///
    /// Example:
    ///
    ///  - `6`: Set the duration to 6ms, allows one request per 6 ms.
    ///
    ///  - `6/2`: Set the duration to 6ms and request_count to 2,
    ///    allows 2 requests per 6ms.
    ///
    /// Both duration and request count must not be 0.
    #[clap(help_heading = "Overrides", long, default_value_t = RateLimit::default())]
    pub rate_limit: RateLimit,

    /// Specify the strategies to be used,
    /// binstall would run the strategies specified in order.
    ///
    /// Default value is "release,quick-install,compile".
    #[clap(help_heading = "Overrides", long)]
    pub strategies: Option<Vec<Strategy>>,

    /// Disable the strategies specified.
    /// If a strategy is specified in `--strategies` and `--disable-strategies`,
    /// then it will be removed.
    #[clap(help_heading = "Overrides", long)]
    pub disable_strategies: Option<Vec<Strategy>>,

    /// Disable symlinking / versioned updates.
    ///
    /// By default, Binstall will install a binary named `<name>-<version>` in the install path, and
    /// either symlink or copy it to (depending on platform) the plain binary name. This makes it
    /// possible to have multiple versions of the same binary, for example for testing or rollback.
    ///
    /// Pass this flag to disable this behavior.
    #[clap(help_heading = "Options", long)]
    pub no_symlinks: bool,

    /// Dry run, fetch and show changes without installing binaries.
    #[clap(help_heading = "Options", long)]
    pub dry_run: bool,

    /// Disable interactive mode / confirmation prompts.
    #[clap(help_heading = "Options", long)]
    pub no_confirm: bool,

    /// Do not cleanup temporary files.
    #[clap(help_heading = "Options", long)]
    pub no_cleanup: bool,

    /// Install binaries in a custom location.
    ///
    /// By default, binaries are installed to the global location `$CARGO_HOME/bin`, and global
    /// metadata files are updated with the package information. Specifying another path here
    /// switches over to a "local" install, where binaries are installed at the path given, and the
    /// global metadata files are not updated.
    #[clap(help_heading = "Options", long)]
    pub install_path: Option<PathBuf>,

    /// Install binaries with a custom cargo root.
    ///
    /// By default, we use `$CARGO_INSTALL_ROOT` or `$CARGO_HOME` as the
    /// cargo root and global metadata files are updated with the
    /// package information.
    ///
    /// Specifying another path here would install the binaries and update
    /// the metadata files inside the path you specified.
    ///
    /// NOTE that `--install-path` takes precedence over this option.
    #[clap(help_heading = "Options", long)]
    pub roots: Option<PathBuf>,

    /// Deprecated, here for back-compat only. Secure is now on by default.
    #[clap(hide(true), long)]
    pub secure: bool,

    /// Force a crate to be installed even if it is already installed.
    #[clap(help_heading = "Options", long)]
    pub force: bool,

    /// Require a minimum TLS version from remote endpoints.
    ///
    /// The default is not to require any minimum TLS version, and use the negotiated highest
    /// version available to both this client and the remote server.
    #[clap(help_heading = "Options", long, value_enum, value_name = "VERSION")]
    pub min_tls_version: Option<TLSVersion>,

    /// Print version information
    #[clap(help_heading = "Meta", short = 'V')]
    pub version: bool,

    /// Utility log level
    ///
    /// Set to `trace` to print very low priority, often extremely
    /// verbose information.
    ///
    /// Set to `debug` when submitting a bug report.
    ///
    /// Set to `info` to only print useful information.
    ///
    /// Set to `warn` to only print on hazardous situations.
    ///
    /// Set to `error` to only print serious errors.
    ///
    /// Set to `off` to disable logging completely, this will also
    /// disable output from `cargo-install`.
    #[clap(
        help_heading = "Meta",
        long,
        default_value = "info",
        value_name = "LEVEL"
    )]
    pub log_level: LevelFilter,

    /// Equivalent to setting `log_level` to `off`.
    ///
    /// This would override the `log_level`.
    #[clap(help_heading = "Meta", short, long)]
    pub quiet: bool,
}

#[derive(Debug, Copy, Clone, ValueEnum)]
pub enum TLSVersion {
    #[clap(name = "1.2")]
    Tls1_2,
    #[clap(name = "1.3")]
    Tls1_3,
}

impl From<TLSVersion> for Version {
    fn from(ver: TLSVersion) -> Self {
        match ver {
            TLSVersion::Tls1_2 => Version::TLS_1_2,
            TLSVersion::Tls1_3 => Version::TLS_1_3,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct RateLimit {
    pub duration: NonZeroU64,
    pub request_count: NonZeroU64,
}

impl fmt::Display for RateLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.duration, self.request_count)
    }
}

impl FromStr for RateLimit {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if let Some((first, second)) = s.split_once('/') {
            Self {
                duration: first.parse()?,
                request_count: second.parse()?,
            }
        } else {
            Self {
                duration: s.parse()?,
                ..Default::default()
            }
        })
    }
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            duration: NonZeroU64::new(5).unwrap(),
            request_count: NonZeroU64::new(1).unwrap(),
        }
    }
}

/// Strategy for installing the package
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, ValueEnum)]
pub enum Strategy {
    /// Attempt to download official pre-built artifacts using
    /// information provided in `Cargo.toml`.
    Release,
    /// Query third-party QuickInstall for the crates.
    QuickInstall,
    /// Build the crates from source using `cargo-build`.
    Compile,
}

pub fn parse() -> Result<Args, BinstallError> {
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
    let mut opts = Args::parse_from(args);
    if opts.quiet {
        opts.log_level = LevelFilter::Off;
    }

    if opts.crate_names.len() > 1 {
        let option = if opts.version_req.is_some() {
            "version"
        } else if opts.manifest_path.is_some() {
            "manifest-path"
        } else {
            ""
        };

        if !option.is_empty() {
            return Err(BinstallError::OverrideOptionUsedWithMultiInstall { option });
        }
    }

    Ok(opts)
}
