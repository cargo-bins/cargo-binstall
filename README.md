# Cargo B(inary)Install

`cargo binstall` provides a low-complexity mechanism for installing rust binaries as an alternative to building from source (via `cargo install`) or manually downloading packages. This is intended to work with existing CI artifacts and infrastructure, and with minimal overhead for package maintainers.

`binstall` works by fetching the crate information from `crates.io`, then searching the linked `repository` for matching releases and artifacts, with fallbacks to [quickinstall](https://github.com/alsuren/cargo-quickinstall) and finally `cargo install` if these are not found.
To support `binstall` maintainers must add configuration values to `Cargo.toml` to allow the tool to locate the appropriate binary package for a given version and target. See [SUPPORT.md](./SUPPORT.md) for more detail.

## Status

[![CI build](https://github.com/cargo-bins/cargo-binstall/actions/workflows/ci.yml/badge.svg)](https://github.com/cargo-bins/cargo-binstall/actions)
[![GitHub tag](https://img.shields.io/github/tag/cargo-bins/cargo-binstall.svg)](https://github.com/cargo-bins/cargo-binstall)
[![Crates.io](https://img.shields.io/crates/v/cargo-binstall.svg)](https://crates.io/crates/cargo-binstall)

You probably want to **[see this page as it was when the latest version was published](https://crates.io/crates/cargo-binstall)** for accurate documentation.

## Installation

Here are the one-liners for installing pre-compiled `cargo-binstall` binary from release on Linux and macOS:

```
curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
```

And the one-liner for installing a pre-compiled `cargo-binstall` binary from release on Windows (x86_64 and aarch64):

```
Set-ExecutionPolicy Unrestricted -Scope Process; iex (iwr "https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.ps1").Content
```

To get started _using_ `cargo-binstall` first install the binary (either via `cargo install cargo-binstall` or by downloading a pre-compiled [release](https://github.com/cargo-bins/cargo-binstall/releases)), then extract it using `tar` or `unzip` and move it into `$HOME/.cargo/bin`.
We recommend using the pre-compiled ones because we optimize those more than a standard source build does.

| OS      | Arch    | URL                                                          |
| ------- | ------- | ------------------------------------------------------------ |
| linux   | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-unknown-linux-musl.tgz |
| linux   | armv7   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-armv7-unknown-linux-musleabihf.tgz |
| linux   | arm64   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-unknown-linux-musl.tgz |
| macos   | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-apple-darwin.zip |
| macos   | m1      | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-apple-darwin.zip |
| macos   | universal | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-universal-apple-darwin.zip |
| windows | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-pc-windows-msvc.zip |
| windows | arm64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-pc-windows-msvc.zip |

We also provide pre-built artifacts with debuginfo for Linux and Mac.
These artifacts are suffixed with `.full.tgz` on Linux and `.full.zip` on Mac and Windows.

To upgrade cargo-binstall, use `cargo binstall cargo-binstall`!

## Usage

Supported packages can be installed using `cargo binstall NAME` where `NAME` is the crates.io package name.

Package versions and targets may be specified using the `--version` and `--target` arguments respectively, and will be installed into `$HOME/.cargo/bin` by default. For additional options please see `cargo binstall --help`.

```
[garry] ➜  ~ cargo binstall radio-sx128x --version 0.14.1-alpha.5
21:14:15 [INFO] Resolving package: 'radio-sx128x'
21:14:18 [INFO] This will install the following binaries:
21:14:18 [INFO]   - sx128x-util (sx128x-util-x86_64-apple-darwin -> /Users/ryankurte/.cargo/bin/sx128x-util-v0.14.1-alpha.5)
21:14:18 [INFO] And create (or update) the following symlinks:
21:14:18 [INFO]   - sx128x-util (/Users/ryankurte/.cargo/bin/sx128x-util-v0.14.1-alpha.5 -> /Users/ryankurte/.cargo/bin/sx128x-util)
21:14:18 [INFO] Do you wish to continue? yes/[no]
? yes
21:14:20 [INFO] Installing binaries...
21:14:21 [INFO] Done in 6.212736s
```

## Unsupported crates

Nowadays, `cargo-binstall` is smart enough. All you need just passing the crate name.

```shell
cargo binstall --no-confirm --no-symlinks cargo-edit cargo-watch cargo-tarpaulin \
    watchexec-cli cargo-outdated just fnm broot stylua
```

If your favorite package fails to install, you can instead specify the `pkg-url`, `bin-dir`, and `pkg-fmt` at the command line, with values as documented in [SUPPORT.md](./SUPPORT.md).

For example:

```shell
$ cargo-binstall \
  --pkg-url="{ repo }/releases/download/{ version }/{ name }-{ version }-{ target }.{ archive-format }" \
  --pkg-fmt="txz" \
  crate_name
```

## Upgrade installed crates

The most ergonomic way to upgrade the installed crates is with [`cargo-update`](https://github.com/nabijaczleweli/cargo-update). `cargo-update` automatically uses `cargo-binstall` to install the updates if `cargo-binstall` is present.

Supported crates such as `cargo-binstall` itself can also be updated with `cargo-binstall` as in the example in [Installation](#installation) above.

## Signatures

We have initial, limited [support](./SIGNING.md) for maintainers to specify a signing public key and where to find package signatures.
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

We have [initial support](./SIGNING.md) for verifying signatures, but not a lot of the ecosystem produces signatures at the moment.
See [#1](https://github.com/cargo-bins/cargo-binstall/issues/1) to discuss more on this.

We always pull the metadata from crates.io over HTTPS, and verify the checksum of the crate tar.
We also enforce using HTTPS with TLS >= 1.2 for the actual download of the package files.

Compared to something like a `curl ... | sh` script, we're not running arbitrary code, but of course the crate you're downloading a package for might itself be malicious!

### What do the error codes mean?
You can find a full description of errors including exit codes here: <https://docs.rs/binstalk/latest/binstalk/errors/enum.BinstallError.html>

### Can I use it in CI?
Yes! We have two options, both for GitHub Actions:

1. For full featured use, we recommend the excellent [taiki-e/install-action](https://github.com/marketplace/actions/install-development-tools), which has explicit support for selected tools and uses `cargo-binstall` for everything else.
2. We provide a first-party, minimal action that _only_ installs the tool:
```yml
  - uses: cargo-bins/cargo-binstall@main
```

### Are debug symbols available?
Yes!
Extra pre-built packages with a `.full` suffix are available and contain split debuginfo, documentation files, and extra binaries like the `detect-wasi` utility.

---

If you have ideas/contributions or anything is not working the way you expect (in which case, please include an output with `--log-level debug`) and feel free to open an issue or PR.
