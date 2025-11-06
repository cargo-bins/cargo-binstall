# Command-Line Help for `cargo-binstall`

This document contains the help content for the `cargo-binstall` command-line program.

**Command Overview:**

* [`cargo-binstall`↴](#cargo-binstall)

## `cargo-binstall`

Install a Rust binary... from binaries!

**Usage:** `cargo-binstall [OPTIONS] [crate[@version]]...`

License: GPLv3. Source available at https://github.com/cargo-bins/cargo-binstall

Some crate installation strategies may collect anonymized usage statistics by default. If you prefer not to participate on such data collection, you can opt out by using the `--disable-telemetry` flag or its associated environment variable. For more details about this data collection, please refer to the mentioned flag or the project's README file

###### **Arguments:**

* `<crate[@version]>` — Packages to install.

   Syntax: `crate[@version]`

   Each value is either a crate name alone, or a crate name followed by @ and the version to install. The version syntax is as with the --version option.

   When multiple names are provided, the --version option and override option `--manifest-path` and `--git` are unavailable due to ambiguity.

   If duplicate names are provided, the last one (and its version requirement) is kept.

###### **Options:**

* `--version <VERSION>` — Package version to install.

   Takes either an exact semver version or a semver version requirement expression, which will be resolved to the highest matching version available.

   Cannot be used when multiple packages are installed at once, use the attached version syntax in that case.
* `--targets <TRIPLE>` — Override binary target set.

   Binstall is able to look for binaries for several targets, installing the first one it finds in the order the targets were given. For example, on a 64-bit glibc Linux distribution, the default is to look first for a `x86_64-unknown-linux-gnu` binary, then for a `x86_64-unknown-linux-musl` binary. However, on a musl system, the gnu version will not be considered.

   This option takes a comma-separated list of target triples, which will be tried in order. They override the default list, which is detected automatically from the current platform.

   If falling back to installing from source, the first target will be used.
* `--bin <BINARY>` — Install only the specified binaries.

   This mirrors the equivalent argument in `cargo install --bin`.

   If omitted, all binaries are installed.
* `--manifest-path <PATH>` — Override Cargo.toml package manifest path.

   This skips searching crates.io for a manifest and uses the specified path directly, useful for debugging and when adding Binstall support. This may be either the path to the folder containing a Cargo.toml file, or the Cargo.toml file itself.

   This option cannot be used with `--git`.
* `--git <URL>` — Override how to fetch Cargo.toml package manifest.

   This skips searching crates.io and instead clones the repository specified and runs as if `--manifest-path $cloned_repo` is passed to binstall.

   This option cannot be used with `--manifest-path`.
* `--bin-dir <BIN_DIR>` — Path template for binary files in packages

   Overrides the Cargo.toml package manifest bin-dir.
* `--pkg-fmt <PKG_FMT>` — Format for package downloads

   Overrides the Cargo.toml package manifest pkg-fmt.

   The available package formats are:

   - tar: download format is TAR (uncompressed)

   - tbz2: Download format is TAR + Bzip2

   - tgz: Download format is TGZ (TAR + GZip)

   - txz: Download format is TAR + XZ

   - tzstd: Download format is TAR + Zstd

   - zip: Download format is Zip

   - bin: Download format is raw / binary
* `--pkg-url <TEMPLATE>` — URL template for package downloads

   Overrides the Cargo.toml package manifest pkg-url.
* `--rate-limit <LIMIT>` — Override the rate limit duration.

   By default, cargo-binstall allows one request per 10 ms.

   Example:

   - `6`: Set the duration to 6ms, allows one request per 6 ms.

   - `6/2`: Set the duration to 6ms and request_count to 2, allows 2 requests per 6ms.

   Both duration and request count must not be 0.

  Default value: `10/1`
* `--strategies <STRATEGIES>` — Specify the strategies to be used, binstall will run the strategies specified in order.

   If this option is specified, then cargo-binstall will ignore `disabled-strategies` in `package.metadata` in the cargo manifest of the installed packages.

   Default value is "crate-meta-data,quick-install,compile".

  Possible values: `crate-meta-data`, `quick-install`, `compile`

* `--disable-strategies <STRATEGIES>` — Disable the strategies specified. If a strategy is specified in `--strategies` and `--disable-strategies`, then it will be removed.

   If `--strategies` is not specified, then the strategies specified in this option will be merged with the  disabled-strategies` in `package.metadata` in the cargo manifest of the installed packages.

  Possible values: `crate-meta-data`, `quick-install`, `compile`

* `--no-discover-github-token` — If `--github-token` or environment variable `GITHUB_TOKEN`/`GH_TOKEN` is not specified, then cargo-binstall will try to extract github token from `$HOME/.git-credentials` or `$HOME/.config/gh/hosts.yml` by default.

   This option can be used to disable that behavior.
* `--maximum-resolution-timeout <TIMEOUT>` — Maximum time each resolution (one for each possible target and each strategy), in seconds

  Default value: `15`
* `--no-symlinks` — This flag is now enabled by default thus a no-op.

   By default, Binstall will install a binary as-is in the install path.

  Default value: `true`
* `--dry-run` — Dry run, fetch and show changes without installing binaries
* `-y`, `--no-confirm` — Disable interactive mode / confirmation prompts
* `--no-cleanup` — Do not cleanup temporary files
* `--continue-on-failure` — Continue installing other crates even if one of the crate failed to install
* `--no-track` — By default, binstall keeps track of the installed packages with metadata files stored in the installation root directory.

   This flag tells binstall not to use or create that file.

   With this flag, binstall will refuse to overwrite any existing files unless the `--force` flag is used.

   This also disables binstall’s ability to protect against multiple concurrent invocations of binstall installing at the same time.

   This flag will also be passed to `cargo-install` if it is invoked.
* `--disable-telemetry` — Disable statistics collection on popular crates.

   Strategy quick-install (can be disabled via --disable-strategies) collects statistics of popular crates by default, by sending the crate, version, target and status to https://cargo-quickinstall-stats-server.fly.dev/record-install
* `--install-path <PATH>` — Install prebuilt binaries in a custom location.

   By default, binaries are installed to the global location `$CARGO_HOME/bin`, and global metadata files are updated with the package information. Specifying another path here switches over to a "local" install, where binaries are installed at the path given, and the global metadata files are not updated.

   This option has no effect if the package is installed from source. To install a package from source to a specific path, without Cargo metadata use `--root <PATH> --no-track`.
* `--root <ROOT>` — Install binaries with a custom cargo root.

   By default, we use `$CARGO_INSTALL_ROOT` or `$CARGO_HOME` as the cargo root and global metadata files are updated with the package information.

   Specifying another path here would install the binaries and update the metadata files inside the path you specified.

   NOTE that `--install-path` takes precedence over this option.
* `--index <INDEX>` — The URL of the registry index to use.

   Cannot be used with `--registry`.
* `--registry <REGISTRY>` — Name of the registry to use. Registry names are defined in Cargo configuration files <https://doc.rust-lang.org/cargo/reference/config.html>.

   If not specified on the command line or via an environment variable, the default registry is used. This is controlled by the `registry.default` key in `.cargo/config.toml`. If that key is not set, the default is `crates.io`.

   If a registry name is provided, Cargo first checks the environment variable `CARGO_REGISTRIES_{registry_name}_INDEX` for the index URL. If that is not set, it falls back to the `registries.<name>.index` key in `.cargo/config.toml`.

   Cannot be combined with `--index`.
* `--locked` — This option will be passed through to all `cargo-install` invocations.

   It will require `Cargo.lock` to be up to date.
* `--force` — Force a crate to be installed even if it is already installed
* `--min-tls-version <VERSION>` — Require a minimum TLS version from remote endpoints.

   The default is not to require any minimum TLS version, and use the negotiated highest version available to both this client and the remote server.

  Possible values: `1.2`, `1.3`

* `--root-certificates <PATH>` — Specify the root certificates to use for https connections, in addition to default system-wide ones
* `--json-output` — Print logs in json format to be parsable
* `--github-token <TOKEN>` — Provide the github token for accessing the restful API of api.github.com

   Fallback to environment variable `GITHUB_TOKEN` if this option is not specified (which is also shown by clap's auto generated doc below), or try environment variable `GH_TOKEN`, which is also used by `gh` cli.

   If none of them is present, then binstall will try to extract github token from `$HOME/.git-credentials` or `$HOME/.config/gh/hosts.yml` unless `--no-discover-github-token` is specified.
* `--only-signed` — Only install packages that are signed

   The default is to verify signatures if they are available, but to allow unsigned packages as well.
* `--skip-signatures` — Don't check any signatures

   The default is to verify signatures if they are available. This option disables that behaviour entirely, which will also stop downloading signature files in the first place.

   Note that this is insecure and not recommended outside of testing.
* `--settings <SETTINGS>` — Custom settings file

   The default is to read a binstall.toml file from CARGO_HOME or the cargo root directory.

   If a file is not found at the path provided, one will be created with the defaults.
* `-V` — Print version information
* `--log-level <LEVEL>` — Utility log level

   Set to `trace` to print very low priority, often extremely verbose information.

   Set to `debug` when submitting a bug report.

   Set to `info` to only print useful information.

   Set to `warn` to only print on hazardous situations.

   Set to `error` to only print serious errors.

   Set to `off` to disable logging completely, this will also disable output from `cargo-install`.

   If `--log-level` is not specified on cmdline, then cargo-binstall will try to read environment variable `BINSTALL_LOG_LEVEL` and interpret it as a log-level.
* `-v`, `--verbose` — Implies `--log-level debug` and it can also be used with `--version` to print out verbose information,
* `-q`, `--quiet` — Equivalent to setting `log_level` to `off`.

   This would override the `log_level`.



<hr/>

<small><i>
    This document was generated automatically by
    <a href="https://crates.io/crates/clap-markdown"><code>clap-markdown</code></a>.
</i></small>

