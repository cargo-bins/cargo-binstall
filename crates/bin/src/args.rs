use std::{
    env,
    ffi::OsString,
    fmt, mem,
    num::{NonZeroU16, NonZeroU64, ParseIntError},
    path::PathBuf,
    str::FromStr,
};

use binstalk::{
    helpers::remote,
    manifests::cargo_toml_binstall::PkgFmt,
    ops::resolve::{CrateName, VersionReqExt},
    registry::Registry,
};
use binstalk_manifests::cargo_toml_binstall::{PkgOverride, Strategy};
use clap::{builder::PossibleValue, error::ErrorKind, CommandFactory, Parser, ValueEnum};
use compact_str::CompactString;
use log::LevelFilter;
use semver::VersionReq;
use serde::{Deserialize, Serialize};
use strum::EnumCount;
use zeroize::Zeroizing;

#[derive(Debug, Parser)]
#[clap(
    version,
    about = "Install a Rust binary... from binaries!",
    after_long_help =
        "License: GPLv3. Source available at https://github.com/cargo-bins/cargo-binstall\n\n\
        Some crate installation strategies may collect anonymized usage statistics by default. \
        If you prefer not to participate on such data collection, you can opt out by using the \
        `--disable-telemetry` flag or its associated environment variable. For more details \
        about this data collection, please refer to the mentioned flag or the project's README \
        file",
    arg_required_else_help(true),
    // Avoid conflict with version_req
    disable_version_flag(true),
    styles = clap_cargo::style::CLAP_STYLING,
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
    /// If duplicate names are provided, the last one (and its version requirement)
    /// is kept.
    #[clap(
        help_heading = "Package selection",
        value_name = "crate[@version]",
        required_unless_present_any = ["version", "self_install", "help"],
    )]
    pub(crate) crate_names: Vec<CrateName>,

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
        value_parser(VersionReq::parse_from_cli),
        value_name = "VERSION"
    )]
    pub(crate) version_req: Option<VersionReq>,

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
        value_name = "TRIPLE",
        env = "CARGO_BUILD_TARGET"
    )]
    pub(crate) targets: Option<Vec<String>>,

    /// Install only the specified binaries.
    ///
    /// This mirrors the equivalent argument in `cargo install --bin`.
    ///
    /// If omitted, all binaries are installed.
    #[clap(
        help_heading = "Package selection",
        long,
        value_name = "BINARY",
        num_args = 1..,
        action = clap::ArgAction::Append
    )]
    pub(crate) bin: Option<Vec<CompactString>>,

    /// Override Cargo.toml package manifest path.
    ///
    /// This skips searching crates.io for a manifest and uses the specified path directly, useful
    /// for debugging and when adding Binstall support. This may be either the path to the folder
    /// containing a Cargo.toml file, or the Cargo.toml file itself.
    ///
    /// This option cannot be used with `--git`.
    #[clap(help_heading = "Overrides", long, value_name = "PATH")]
    pub(crate) manifest_path: Option<PathBuf>,

    #[cfg(feature = "git")]
    /// Override how to fetch Cargo.toml package manifest.
    ///
    /// This skips searching crates.io and instead clones the repository specified and
    /// runs as if `--manifest-path $cloned_repo` is passed to binstall.
    ///
    /// This option cannot be used with `--manifest-path`.
    #[clap(
        help_heading = "Overrides",
        long,
        conflicts_with("manifest_path"),
        value_name = "URL"
    )]
    pub(crate) git: Option<binstalk::registry::GitUrl>,

    /// Path template for binary files in packages
    ///
    /// Overrides the Cargo.toml package manifest bin-dir.
    #[clap(help_heading = "Overrides", long)]
    pub(crate) bin_dir: Option<String>,

    /// Format for package downloads
    ///
    /// Overrides the Cargo.toml package manifest pkg-fmt.
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
    pub(crate) pkg_fmt: Option<PkgFmt>,

    /// URL template for package downloads
    ///
    /// Overrides the Cargo.toml package manifest pkg-url.
    #[clap(help_heading = "Overrides", long, value_name = "TEMPLATE")]
    pub(crate) pkg_url: Option<String>,

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
    #[clap(
        help_heading = "Overrides",
        long,
        default_value_t = RateLimit::default(),
        env = "BINSTALL_RATE_LIMIT",
        value_name = "LIMIT",
    )]
    pub(crate) rate_limit: RateLimit,

    /// Specify the strategies to be used,
    /// binstall will run the strategies specified in order.
    ///
    /// If this option is specified, then cargo-binstall will ignore
    /// `disabled-strategies` in `package.metadata` in the cargo manifest
    /// of the installed packages.
    ///
    /// Default value is "crate-meta-data,quick-install,compile".
    #[clap(
        help_heading = "Overrides",
        long,
        value_delimiter(','),
        env = "BINSTALL_STRATEGIES"
    )]
    pub(crate) strategies: Vec<StrategyWrapped>,

    /// Disable the strategies specified.
    /// If a strategy is specified in `--strategies` and `--disable-strategies`,
    /// then it will be removed.
    ///
    /// If `--strategies` is not specified, then the strategies specified in this
    /// option will be merged with the  disabled-strategies` in `package.metadata`
    /// in the cargo manifest of the installed packages.
    #[clap(
        help_heading = "Overrides",
        long,
        value_delimiter(','),
        env = "BINSTALL_DISABLE_STRATEGIES",
        value_name = "STRATEGIES"
    )]
    pub(crate) disable_strategies: Vec<StrategyWrapped>,

    /// If `--github-token` or environment variable `GITHUB_TOKEN`/`GH_TOKEN`
    /// is not specified, then cargo-binstall will try to extract github token from
    /// `$HOME/.git-credentials` or `$HOME/.config/gh/hosts.yml` by default.
    ///
    /// This option can be used to disable that behavior.
    #[clap(
        help_heading = "Overrides",
        long,
        env = "BINSTALL_NO_DISCOVER_GITHUB_TOKEN"
    )]
    pub(crate) no_discover_github_token: bool,

    /// Maximum time each resolution (one for each possible target and each strategy), in seconds.
    #[clap(
        help_heading = "Overrides",
        long,
        env = "BINSTALL_MAXIMUM_RESOLUTION_TIMEOUT",
        default_value_t = NonZeroU16::new(15).unwrap(),
        value_name = "TIMEOUT"
    )]
    pub(crate) maximum_resolution_timeout: NonZeroU16,

    /// This flag is now enabled by default thus a no-op.
    ///
    /// By default, Binstall will install a binary as-is in the install path.
    #[clap(help_heading = "Options", long, default_value_t = true)]
    pub(crate) no_symlinks: bool,

    /// Dry run, fetch and show changes without installing binaries.
    #[clap(help_heading = "Options", long)]
    pub(crate) dry_run: bool,

    /// Disable interactive mode / confirmation prompts.
    #[clap(
        help_heading = "Options",
        short = 'y',
        long,
        env = "BINSTALL_NO_CONFIRM"
    )]
    pub(crate) no_confirm: bool,

    /// Do not cleanup temporary files.
    #[clap(help_heading = "Options", long)]
    pub(crate) no_cleanup: bool,

    /// Continue installing other crates even if one of the crate failed to install.
    #[clap(help_heading = "Options", long)]
    pub(crate) continue_on_failure: bool,

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
    pub(crate) no_track: bool,

    /// Disable statistics collection on popular crates.
    ///
    /// Strategy quick-install (can be disabled via --disable-strategies) collects
    /// statistics of popular crates by default, by sending the crate, version, target
    /// and status to https://cargo-quickinstall-stats-server.fly.dev/record-install
    #[clap(help_heading = "Options", long, env = "BINSTALL_DISABLE_TELEMETRY")]
    pub(crate) disable_telemetry: bool,

    /// Install prebuilt binaries in a custom location.
    ///
    /// By default, binaries are installed to the global location `$CARGO_HOME/bin`, and global
    /// metadata files are updated with the package information. Specifying another path here
    /// switches over to a "local" install, where binaries are installed at the path given, and the
    /// global metadata files are not updated.
    ///
    /// This option has no effect if the package is installed from source. To install a package
    /// from source to a specific path, without Cargo metadata use `--root <PATH> --no-track`.
    #[clap(help_heading = "Options", long, value_name = "PATH")]
    pub(crate) install_path: Option<PathBuf>,

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
    pub(crate) root: Option<PathBuf>,

    /// The URL of the registry index to use.
    ///
    /// Cannot be used with `--registry`.
    #[clap(help_heading = "Options", long)]
    pub(crate) index: Option<Registry>,

    /// Name of the registry to use. Registry names are defined in Cargo
    /// configuration files <https://doc.rust-lang.org/cargo/reference/config.html>.
    ///
    /// If not specified on the command line or via an environment variable, the
    /// default registry is used. This is controlled by the `registry.default` key
    /// in `.cargo/config.toml`. If that key is not set, the default is `crates.io`.
    ///
    /// If a registry name is provided, Cargo first checks the environment variable
    /// `CARGO_REGISTRIES_{registry_name}_INDEX` for the index URL. If that is not
    /// set, it falls back to the `registries.<name>.index` key in `.cargo/config.toml`.
    ///
    /// Cannot be combined with `--index`.
    #[clap(
        help_heading = "Options",
        long,
        env = "CARGO_REGISTRY_DEFAULT",
        conflicts_with("index")
    )]
    pub(crate) registry: Option<CompactString>,

    /// This option will be passed through to all `cargo-install` invocations.
    ///
    /// It will require `Cargo.lock` to be up to date.
    #[clap(help_heading = "Options", long)]
    pub(crate) locked: bool,

    /// Deprecated, here for back-compat only. Secure is now on by default.
    #[clap(hide(true), long)]
    pub(crate) secure: bool,

    /// Force a crate to be installed even if it is already installed.
    #[clap(help_heading = "Options", long)]
    pub(crate) force: bool,

    /// Require a minimum TLS version from remote endpoints.
    ///
    /// The default is not to require any minimum TLS version, and use the negotiated highest
    /// version available to both this client and the remote server.
    #[clap(help_heading = "Options", long, value_enum, value_name = "VERSION")]
    pub(crate) min_tls_version: Option<TLSVersion>,

    /// Specify the root certificates to use for https connections,
    /// in addition to default system-wide ones.
    #[clap(
        help_heading = "Options",
        long,
        env = "BINSTALL_HTTPS_ROOT_CERTS",
        value_name = "PATH"
    )]
    pub(crate) root_certificates: Vec<PathBuf>,

    /// Print logs in json format to be parsable.
    #[clap(help_heading = "Options", long)]
    pub json_output: bool,

    /// Provide the github token for accessing the restful API of api.github.com
    ///
    /// Fallback to environment variable `GITHUB_TOKEN` if this option is not
    /// specified (which is also shown by clap's auto generated doc below), or
    /// try environment variable `GH_TOKEN`, which is also used by `gh` cli.
    ///
    /// If none of them is present, then binstall will try to extract github
    /// token from `$HOME/.git-credentials` or `$HOME/.config/gh/hosts.yml`
    /// unless `--no-discover-github-token` is specified.
    #[clap(
        help_heading = "Options",
        long,
        env = "GITHUB_TOKEN",
        value_name = "TOKEN"
    )]
    pub(crate) github_token: Option<GithubToken>,

    /// Only install packages that are signed
    ///
    /// The default is to verify signatures if they are available, but to allow
    /// unsigned packages as well.
    #[clap(help_heading = "Options", long)]
    pub(crate) only_signed: bool,

    /// Don't check any signatures
    ///
    /// The default is to verify signatures if they are available. This option
    /// disables that behaviour entirely, which will also stop downloading
    /// signature files in the first place.
    ///
    /// Note that this is insecure and not recommended outside of testing.
    #[clap(help_heading = "Options", long, conflicts_with = "only_signed")]
    pub(crate) skip_signatures: bool,

    /// Custom settings file
    ///
    /// The default is to read a binstall.toml file from CARGO_HOME or the cargo root directory.
    ///
    /// If a file is not found at the path provided, one will be created with the defaults.
    #[clap(help_heading = "Options", long)]
    pub(crate) settings: Option<PathBuf>,

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

    /// Implies `--log-level debug` and it can also be used with `--version`
    /// to print out verbose information,
    #[clap(help_heading = "Meta", short, long)]
    pub verbose: bool,

    /// Equivalent to setting `log_level` to `off`.
    ///
    /// This would override the `log_level`.
    #[clap(help_heading = "Meta", short, long, conflicts_with("verbose"))]
    pub(crate) quiet: bool,

    #[clap(long, hide(true))]
    pub(crate) self_install: bool,

    #[cfg(feature = "clap-markdown")]
    #[clap(long, hide = true)]
    pub(crate) markdown_help: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct GithubToken(pub(crate) Zeroizing<Box<str>>);

impl From<&str> for GithubToken {
    fn from(s: &str) -> Self {
        Self(Zeroizing::new(s.into()))
    }
}

#[derive(Debug, Copy, Clone, ValueEnum)]
pub(crate) enum TLSVersion {
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
pub(crate) struct RateLimit {
    pub(crate) duration: NonZeroU16,
    pub(crate) request_count: NonZeroU64,
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
            duration: NonZeroU16::new(10).unwrap(),
            request_count: NonZeroU64::new(1).unwrap(),
        }
    }
}

/// Strategy for installing the package
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct StrategyWrapped(pub(crate) Strategy);

impl StrategyWrapped {
    const VARIANTS: &'static [Self; 3] = &[
        Self(Strategy::CrateMetaData),
        Self(Strategy::QuickInstall),
        Self(Strategy::Compile),
    ];
}

impl ValueEnum for StrategyWrapped {
    fn value_variants<'a>() -> &'a [Self] {
        Self::VARIANTS
    }
    fn to_possible_value(&self) -> Option<PossibleValue> {
        Some(PossibleValue::new(self.0.to_str()))
    }
}

pub fn parse() -> (Args, PkgOverride) {
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

    #[cfg(feature = "clap-markdown")]
    if opts.markdown_help {
        clap_markdown::print_help_markdown::<Args>();
        std::process::exit(0);
    }

    if opts.self_install {
        return (opts, Default::default());
    }

    if opts.log_level.is_none() {
        if let Some(log) = env::var("BINSTALL_LOG_LEVEL")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            opts.log_level = Some(log);
        } else if opts.quiet {
            opts.log_level = Some(LevelFilter::Off);
        } else if opts.verbose {
            opts.log_level = Some(LevelFilter::Debug);
        }
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
        let index = strategy.0 as u8 as usize;
        if is_variant_present[index] {
            new_dup_strategy_err().exit()
        } else {
            is_variant_present[index] = true;
        }
    }

    let ignore_disabled_strategies = !opts.strategies.is_empty();

    // Default strategies if empty
    if opts.strategies.is_empty() {
        opts.strategies = vec![
            StrategyWrapped(Strategy::CrateMetaData),
            StrategyWrapped(Strategy::QuickInstall),
            StrategyWrapped(Strategy::Compile),
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
    }

    // Ensure that Strategy::Compile is specified as the last strategy
    if opts.strategies[..(opts.strategies.len() - 1)].contains(&StrategyWrapped(Strategy::Compile))
    {
        command
            .error(
                ErrorKind::InvalidValue,
                "Compile strategy must be the last one",
            )
            .exit()
    }

    if opts.github_token.is_none() {
        if let Ok(github_token) = env::var("GH_TOKEN") {
            opts.github_token = Some(GithubToken(Zeroizing::new(github_token.into())));
        }
    }
    match opts.github_token.as_ref() {
        Some(token) if token.0.len() < 10 => opts.github_token = None,
        _ => (),
    }

    let cli_overrides = PkgOverride {
        pkg_url: opts.pkg_url.take(),
        pkg_fmt: opts.pkg_fmt.take(),
        bin_dir: opts.bin_dir.take(),
        disabled_strategies: Some(
            mem::take(&mut opts.disable_strategies)
                .into_iter()
                .map(|strategy| strategy.0)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        ignore_disabled_strategies,
        signing: None,
    };

    (opts, cli_overrides)
}

#[cfg(test)]
mod test {
    use strum::VariantArray;

    use super::*;

    #[test]
    fn verify_cli() {
        Args::command().debug_assert()
    }

    #[test]
    fn quickinstall_url_matches() {
        let long_help = Args::command()
            .get_opts()
            .find(|opt| opt.get_long() == Some("disable-telemetry"))
            .unwrap()
            .get_long_help()
            .unwrap()
            .to_string();
        assert!(
            long_help.ends_with(binstalk::QUICKINSTALL_STATS_URL),
            "{}",
            long_help
        );
    }

    const _: () = assert!(Strategy::VARIANTS.len() == StrategyWrapped::VARIANTS.len());
}
