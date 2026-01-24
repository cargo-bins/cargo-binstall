# Cargo B(inary)Install

Binstall provides a low-complexity mechanism for installing Rust binaries as an alternative to building from source (via `cargo install`) or manually downloading packages.
This is intended to work with existing CI artifacts and infrastructure, and with minimal overhead for package maintainers.

Binstall works by fetching the crate information from `crates.io` and searching the linked `repository` for matching releases and artifacts, falling back to the [quickinstall](https://github.com/alsuren/cargo-quickinstall) third-party artifact host, to alternate targets as supported, and finally to `cargo install` as a last resort.

[![CI build](https://github.com/cargo-bins/cargo-binstall/actions/workflows/ci.yml/badge.svg)](https://github.com/cargo-bins/cargo-binstall/actions)
[![GitHub tag](https://img.shields.io/github/tag/cargo-bins/cargo-binstall.svg)](https://github.com/cargo-bins/cargo-binstall/releases)
[![Crates.io](https://img.shields.io/crates/v/cargo-binstall.svg)](https://crates.io/crates/cargo-binstall)

_You may want to [see this page as it was when the latest version was published](https://crates.io/crates/cargo-binstall)._

## Usage

```console
$ cargo binstall radio-sx128x@0.14.1-alpha.5
 INFO resolve: Resolving package: 'radio-sx128x@=0.14.1-alpha.5'
 WARN The package radio-sx128x v0.14.1-alpha.5 (x86_64-unknown-linux-gnu) has been downloaded from github.com
 INFO This will install the following binaries:
 INFO   - sx128x-util (sx128x-util-x86_64-unknown-linux-gnu -> /home/.cargo/bin/sx128x-util)
Do you wish to continue? [yes]/no
? yes
 INFO Installing binaries...
 INFO Done in 2.838798298s
```

Binstall aims to be a drop-in replacement for `cargo install` in many cases, and supports similar options.

For unattended use (e.g. in CI), use the `--no-confirm` flag.
For additional options please see `cargo binstall --help`.

## Installation

### If you already have it

To upgrade cargo-binstall, use `cargo binstall cargo-binstall`!

### Quickly

Here are one-liners for downloading and installing a pre-compiled `cargo-binstall` binary.

#### Linux and macOS

```
curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
```

or if you have [homebrew](https://brew.sh/) installed:

```
brew install cargo-binstall
```

#### Windows

```
Set-ExecutionPolicy Unrestricted -Scope Process; iex (iwr "https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.ps1").Content
```

### Manually

Download the relevant package for your system below, unpack it, and move the `cargo-binstall` executable into `$HOME/.cargo/bin`:

| OS      | Arch    | URL                                                          |
| ------- | ------- | ------------------------------------------------------------ |
| Linux   | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-unknown-linux-musl.tgz |
| Linux   | armv7   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-armv7-unknown-linux-musleabihf.tgz |
| Linux   | arm64   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-unknown-linux-musl.tgz |
| Mac     | Intel   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-apple-darwin.zip |
| Mac     | Apple Silicon | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-apple-darwin.zip |
| Mac     | Universal<br>(both archs) | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-universal-apple-darwin.zip |
| Windows | Intel/AMD | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-pc-windows-msvc.zip |
| Windows | ARM 64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-pc-windows-msvc.zip |

### From source

With a recent [Rust](https://rustup.rs) installed:

```
cargo install cargo-binstall
```

### In GitHub Actions

We provide a first-party, minimal action that installs Binstall:

```yml
  - uses: cargo-bins/cargo-binstall@main
    with:
      version: "1.2.3" # optional; defaults to latest
```

For more features, we recommend the excellent [taiki-e/install-action](https://github.com/marketplace/actions/install-development-tools), which has dedicated support for selected tools and uses Binstall for everything else.

## Companion tools

These are useful *third-party* tools which work well with Binstall.

### [`cargo-update`](https://github.com/nabijaczleweli/cargo-update)

While you can upgrade crates explicitly by running `cargo binstall` again, `cargo-update` takes care of updating all tools as needed.
It automatically uses Binstall to install the updates if it is present.

### [`cargo-run-bin`](https://github.com/dustinblackman/cargo-run-bin)

Binstall and `cargo install` both install tools globally by default, which is fine for system-wide tools.
When installing tooling for a project, however, you may prefer to both scope the tools to that project and control their versions in code.
That's where `cargo-run-bin` comes in, with a dedicated section in your Cargo.toml and a short cargo subcommand.
When Binstall is available, it installs from binary whenever possible... and you can even manage Binstall itself with `cargo-run-bin`!

## Unsupported crates

Binstall is generally smart enough to auto-detect artifacts in most situations.
However, if a package fails to install, you can manually specify the `pkg-url`, `bin-dir`, and `pkg-fmt` as needed at the command line, with values as documented in [SUPPORT.md](https://github.com/cargo-bins/cargo-binstall/blob/main/SUPPORT.md).

```console
$ cargo-binstall \
  --pkg-url="{ repo }/releases/download/{ version }/{ name }-{ version }-{ target }.{ archive-format }" \
  --pkg-fmt="txz" \
  crate_name
```

Maintainers wanting to make their users' life easier can add [explicit Binstall metadata](https://github.com/cargo-bins/cargo-binstall/blob/main/SUPPORT.md) to `Cargo.toml` to locate the appropriate binary package for a given version and target.

## Signatures

We have initial, limited [support](https://github.com/cargo-bins/cargo-binstall/blob/main/SIGNING.md) for maintainers to specify a signing public key and where to find package signatures.
With this enabled, Binstall will download and verify signatures for that package.

You can use `--only-signed` to refuse to install packages if they're not signed.

If you like to live dangerously (please don't use this outside testing), you can use `--skip-signatures` to disable checking or even downloading signatures at all.

## FAQ

### Why use this?
Because `wget`-ing releases is frustrating, `cargo install` takes a not inconsequential portion of forever on constrained devices, and often putting together actual _packages_ is overkill.

### Why use the cargo manifest?
Crates already have these, and they already contain a significant portion of the required information.
Also, there's this great and woefully underused (IMO) `[package.metadata]` field.

### Is this secure?
Yes and also no?

We have [initial support](https://github.com/cargo-bins/cargo-binstall/blob/main/SIGNING.md) for verifying signatures, but not a lot of the ecosystem produces signatures at the moment.
See [#1](https://github.com/cargo-bins/cargo-binstall/issues/1) to discuss more on this.

We always pull the metadata from crates.io over HTTPS, and verify the checksum of the crate tar.
We also enforce using HTTPS with TLS >= 1.2 for the actual download of the package files.

Compared to something like a `curl ... | sh` script, we're not running arbitrary code, but of course the crate you're downloading a package for might itself be malicious!

### What do the error codes mean?
You can find a full description of errors including exit codes here: <https://docs.rs/binstalk/latest/binstalk/errors/enum.BinstallError.html>

### Are debug symbols available?
Yes!
Extra pre-built packages with a `.full` suffix are available and contain split debuginfo, documentation files, and extra binaries like the `detect-wasi` utility.

## Telemetry collection

Some crate installation strategies may collect anonymized usage statistics by default.
Currently, only the name of the crate to be installed, its version, the target platform triple, and the collecting user agent are sent to endpoints under the `https://cargo-quickinstall-stats-server.fly.dev/record-install` URL when the `quickinstall` artifact host is used.
The maintainers of the `quickinstall` project use this data to determine which crate versions are most worthwhile to build and host.
The aggregated collected telemetry is publicly accessible at <https://alsuren.grafana.net/public-dashboards/12d4ec3edf2548a1850a813e00592b53>.
Should you be interested on it, the backend code for these endpoints can be found at <https://github.com/cargo-bins/cargo-quickinstall/tree/main/stats-server>.

If you prefer not to participate in this data collection, you can opt out by any of the following methods:

- Setting the `--disable-telemetry` flag in the command line interface.
- Setting the `BINSTALL_DISABLE_TELEMETRY` environment variable to `true`.
- Disabling the `quickinstall` strategy with `--disable-strategies quick-install`, or if specifying a list of strategies to use with `--strategies`, avoiding including `quickinstall` in that list.
- Adding `quick-install` to the `disabled-strategies` configuration key in the crate metadata (refer to [the related support documentation](SUPPORT.md#support-for-cargo-binstall) for more details).

---

If you have ideas/contributions or anything is not working the way you expect (in which case, please include an output with `--log-level debug`) and feel free to open an issue or PR.
