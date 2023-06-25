use std::{
    env,
    ffi::OsString,
    fmt,
    num::{NonZeroU64, ParseIntError},
    path::PathBuf,
    str::FromStr,
};

use binstalk::{
    drivers::Registry,
    helpers::remote,
    manifests::cargo_toml_binstall::PkgFmt,
    ops::resolve::{CrateName, VersionReqExt},
};
use clap::{error::ErrorKind, CommandFactory, Parser, ValueEnum};
use compact_str::CompactString;

use log::LevelFilter;
use semver::VersionReq;
use strum::EnumCount;
use strum_macros::EnumCount;

#[derive(Debug, Parser)]
#[clap(
    version,
    about = "Install a Rust binary... from binaries!",
    after_long_help = "License: GPLv3. Source available at https://github.com/cargo-bins/cargo-binstall",
    arg_required_else_help(true),
    // Avoid conflict with version_req
    disable_version_flag(true),
)]
pub struct Args {
    /// Packages to install.
    ///
    /// Syntax: `crate[@version]`
    ///
    /// Each value is either a crate name alone, or a crate name followed by @ and the version to
    /// install. The version syntax is as with the --version option.
    ///
    /// When multiple names are provided, the --version option and override option
    /// `--manifest-path` and `--git` are unavailable due to ambiguity.
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
    ///
    /// This option cannot be used with `--git`.
    #[clap(help_heading = "Overrides", long)]
    pub manifest_path: Option<PathBuf>,

    #[cfg(feature = "git")]
    /// Override how to fetch Cargo.toml package manifest.
    ///
    /// This skip searching crates.io and instead clone the repository specified and
    /// runs as if `--manifest-path $cloned_repo` is passed to binstall.
    ///
    /// This option cannot be used with `--manifest-path`.
    #[clap(help_heading = "Overrides", long, conflicts_with("manifest_path"))]
    pub git: Option<binstalk::helpers::git::GitUrl>,

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
    /// By default, cargo-binstall allows one request per 10 ms.
    ///
    /// Example:
    ///
    ///  - `6`: Set the duration to 6ms, allows one request per 6 ms.
    ///
    ///  - `6/2`: Set the duration to 6ms and request_count to 2,
    ///    allows 2 requests per 6ms.
    ///
    /// Both duration and request count must not be 0.
    #[clap(help_heading = "Overrides", long, default_value_t = RateLimit::default(), env = "BINSTALL_RATE_LIMIT")]
    pub rate_limit: RateLimit,

    /// Specify the strategies to be used,
    /// binstall will run the strategies specified in order.
    ///
    /// Default value is "crate-meta-data,quick-install,compile".
    #[clap(help_heading = "Overrides", long, value_delimiter(','))]
    pub strategies: Vec<Strategy>,

    /// Disable the strategies specified.
    /// If a strategy is specified in `--strategies` and `--disable-strategies`,
    /// then it will be removed.
    #[clap(help_heading = "Overrides", long, value_delimiter(','))]
    pub disable_strategies: Vec<Strategy>,

    /// If `--github-token` or environment variable `GITHUB_TOKEN`/`GH_TOKEN`
    /// is not specified, then cargo-binstall will try to extract github token from
    /// `$HOME/.git-credentials` or `$HOME/.config/gh/hosts.yml` by default.
    ///
    /// This option can be used to disable that behavior.
    #[clap(help_heading = "Overrides", long)]
    pub no_discover_github_token: bool,

    /// This flag is now enabled by default thus a no-op.
    ///
    /// By default, Binstall will install a binary as-is in the install path.
    #[clap(help_heading = "Options", long, default_value_t = true)]
    pub no_symlinks: bool,

    /// Dry run, fetch and show changes without installing binaries.
    #[clap(help_heading = "Options", long)]
    pub dry_run: bool,

    /// Disable interactive mode / confirmation prompts.
    #[clap(help_heading = "Options", short = 'y', long)]
    pub no_confirm: bool,

    /// Do not cleanup temporary files.
    #[clap(help_heading = "Options", long)]
    pub no_cleanup: bool,

    /// By default, binstall keeps track of the installed packages with metadata files
    /// stored in the installation root directory.
    ///
    /// This flag tells binstall not to use or create that file.
    ///
    /// With this flag, binstall will refuse to overwrite any existing files unless the
    /// `--force` flag is used.
    ///
    /// This also disables binstall’s ability to protect against multiple concurrent
    /// invocations of binstall installing at the same time.
    ///
    /// This flag will also be passed to `cargo-install` if it is invoked.
    #[clap(help_heading = "Options", long)]
    pub no_track: bool,

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
    #[clap(help_heading = "Options", long, alias = "roots")]
    pub root: Option<PathBuf>,

    /// The URL of the registry index to use.
    ///
    /// Cannot be used with `--registry`.
    #[clap(help_heading = "Options", long)]
    pub index: Option<Registry>,

    /// Name of the registry to use. Registry names are defined in Cargo config
    /// files <https://doc.rust-lang.org/cargo/reference/config.html>.
    ///
    /// If not specified in cmdline or via environment variable, the default
    /// registry is used, which is defined by the
    /// `registry.default` config key in `.cargo/config.toml` which defaults
    /// to crates-io.
    ///
    /// If it is set, then it will try to read environment variable
    /// `CARGO_REGISTRIES_{registry_name}_INDEX` for index url and fallback to
    /// reading from `registries.<name>.index`.
    ///
    /// Cannot be used with `--index`.
    #[clap(
        help_heading = "Options",
        long,
        env = "CARGO_REGISTRY_DEFAULT",
        conflicts_with("index")
    )]
    pub registry: Option<CompactString>,

    /// This option will be passed through to all `cargo-install` invocations.
    ///
    /// It will require `Cargo.lock` to be up to date.
    #[clap(help_heading = "Options", long)]
    pub locked: bool,

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

    /// Specify the root certificates to use for https connnections,
    /// in addition to default system-wide ones.
    #[clap(help_heading = "Options", long, env = "BINSTALL_HTTPS_ROOT_CERTS")]
    pub root_certificates: Vec<PathBuf>,

    /// Print logs in json format to be parsable.
    #[clap(help_heading = "Options", long)]
    pub json_output: bool,

    /// Provide the github token for accessing the restful API of api.github.com
    ///
    /// Fallback to environment variable `GITHUB_TOKEN` if this option is not
    /// specified (which is also shown by clap's auto generated doc below), or
    /// try environment variable `GH_TOKEN`, which is also used by `gh` cli.
    ///
    /// If none of them is present, then binstal will try to extract github
    /// token from `$HOME/.git-credentials` or `$HOME/.config/gh/hosts.yml`
    /// unless `--no-discover-github-token` is specified.
    #[clap(help_heading = "Options", long, env = "GITHUB_TOKEN")]
    pub github_token: Option<CompactString>,

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
    ///
    /// If `--log-level` is not specified on cmdline, then cargo-binstall
    /// will try to read environment variable `BINSTALL_LOG_LEVEL` and
    /// interpret it as a log-level.
    #[clap(help_heading = "Meta", long, value_name = "LEVEL")]
    pub log_level: Option<LevelFilter>,

    /// Used with `--version` to print out verbose information.
    #[clap(help_heading = "Meta", short, long, default_value_t = false)]
    pub verbose: bool,

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

impl From<TLSVersion> for remote::TLSVersion {
    fn from(ver: TLSVersion) -> Self {
        match ver {
            TLSVersion::Tls1_2 => remote::TLSVersion::TLS_1_2,
            TLSVersion::Tls1_3 => remote::TLSVersion::TLS_1_3,
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
            duration: NonZeroU64::new(10).unwrap(),
            request_count: NonZeroU64::new(1).unwrap(),
        }
    }
}

/// Strategy for installing the package
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, ValueEnum, EnumCount)]
#[repr(u8)]
pub enum Strategy {
    /// Attempt to download official pre-built artifacts using
    /// information provided in `Cargo.toml`.
    CrateMetaData,
    /// Query third-party QuickInstall for the crates.
    QuickInstall,
    /// Build the crates from source using `cargo-build`.
    Compile,
}

pub fn parse() -> Args {
    // Filter extraneous arg when invoked by cargo
    // `cargo run -- --help` gives ["target/debug/cargo-binstall", "--help"]
    // `cargo binstall --help` gives ["/home/ryan/.cargo/bin/cargo-binstall", "binstall", "--help"]
    let mut args: Vec<OsString> = env::args_os().collect();
    let args = if args.get(1).map(|arg| arg == "binstall").unwrap_or_default() {
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

    if let (true, Some(log)) = (
        opts.log_level.is_none(),
        env::var("BINSTALL_LOG_LEVEL")
            .ok()
            .and_then(|s| s.parse().ok()),
    ) {
        opts.log_level = Some(log);
    } else if opts.quiet {
        opts.log_level = Some(LevelFilter::Off);
    }

    // Ensure no conflict
    let mut command = Args::command();

    if opts.crate_names.len() > 1 {
        let option = if opts.version_req.is_some() {
            "version"
        } else if opts.manifest_path.is_some() {
            "manifest-path"
        } else {
            #[cfg(not(feature = "git"))]
            {
                ""
            }

            #[cfg(feature = "git")]
            if opts.git.is_some() {
                "git"
            } else {
                ""
            }
        };

        if !option.is_empty() {
            command
                .error(
                    ErrorKind::ArgumentConflict,
                    format_args!(
                        r#"override option used with multi package syntax.
You cannot use --{option} and specify multiple packages at the same time. Do one or the other."#
                    ),
                )
                .exit();
        }
    }

    // Check strategies for duplicates
    let mut new_dup_strategy_err = || {
        command.error(
            ErrorKind::TooManyValues,
            "--strategies should not contain duplicate strategy",
        )
    };

    if opts.strategies.len() > Strategy::COUNT {
        // If len of strategies is larger than number of variants of Strategy,
        // then there must be duplicates by pigeon hole principle.
        new_dup_strategy_err().exit()
    }

    // Whether specific variant of Strategy is present
    let mut is_variant_present = [false; Strategy::COUNT];

    for strategy in &opts.strategies {
        let index = *strategy as u8 as usize;
        if is_variant_present[index] {
            new_dup_strategy_err().exit()
        } else {
            is_variant_present[index] = true;
        }
    }

    // Default strategies if empty
    if opts.strategies.is_empty() {
        opts.strategies = vec![
            Strategy::CrateMetaData,
            Strategy::QuickInstall,
            Strategy::Compile,
        ];
    }

    // Filter out all disabled strategies
    if !opts.disable_strategies.is_empty() {
        // Since order doesn't matter, we can sort it and remove all duplicates
        // to speedup checking.
        opts.disable_strategies.sort_unstable();
        opts.disable_strategies.dedup();

        // disable_strategies.len() <= Strategy::COUNT, of which is faster
        // to just use [Strategy]::contains rather than
        // [Strategy]::binary_search
        opts.strategies
            .retain(|strategy| !opts.disable_strategies.contains(strategy));

        if opts.strategies.is_empty() {
            command
                .error(ErrorKind::TooFewValues, "You have disabled all strategies")
                .exit()
        }

        // Free disable_strategies as it will not be used again.
        opts.disable_strategies = Vec::new();
    }

    // Ensure that Strategy::Compile is specified as the last strategy
    if opts.strategies[..(opts.strategies.len() - 1)].contains(&Strategy::Compile) {
        command
            .error(
                ErrorKind::InvalidValue,
                "Compile strategy must be the last one",
            )
            .exit()
    }

    if opts.github_token.is_none() {
        if let Ok(github_token) = env::var("GH_TOKEN") {
            opts.github_token = Some(github_token.into());
        } else if !opts.no_discover_github_token {
            if let Some(github_token) = crate::git_credentials::try_from_home() {
                opts.github_token = Some(github_token);
            } else if let Ok(github_token) = gh_token::get() {
                opts.github_token = Some(github_token.into());
            }
        }
    }

    opts
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn verify_cli() {
        Args::command().debug_assert()
    }
}
