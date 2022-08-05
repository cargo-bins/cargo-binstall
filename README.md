# Cargo B(inary)Install

`cargo binstall` provides a low-complexity mechanism for installing rust binaries as an alternative to building from source (via `cargo install`) or manually downloading packages. This is intended to work with existing CI artifacts and infrastructure, and with minimal overhead for package maintainers.

`binstall` works by fetching the crate information from `crates.io`, then searching the linked `repository` for matching releases and artifacts.
To support `binstall` maintainers must add configuration values to `Cargo.toml` to allow the tool to locate the appropriate binary package for a given version and target. See [SUPPORT.md](./SUPPORT.md) for more detail.


## Status

![Build](https://github.com/ryankurte/cargo-binstall/workflows/Rust/badge.svg)
[![GitHub tag](https://img.shields.io/github/tag/ryankurte/cargo-binstall.svg)](https://github.com/ryankurte/cargo-binstall)
[![Crates.io](https://img.shields.io/crates/v/cargo-binstall.svg)](https://crates.io/crates/cargo-binstall)
[![Docs.rs](https://docs.rs/cargo-binstall/badge.svg)](https://docs.rs/cargo-binstall)

## Installation

To get started _using_ `cargo-binstall` first install the binary (either via `cargo install cargo-binstall` or by downloading a pre-compiled [release](https://github.com/ryankurte/cargo-binstall/releases)).


| OS      | Arch    | URL                                                          |
| ------- | ------- | ------------------------------------------------------------ |
| linux   | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-unknown-linux-musl.tgz |
| linux   | armv7   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-unknown-linux-musl.tgz |
| linux   | arm64   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-unknown-linux-musl.tgz |
| macos   | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-apple-darwin.zip |
| macos   | m1      | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-apple-darwin.zip |
| windows | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-pc-windows-msvc.zip |



## Usage

Supported packages can be installed using `cargo binstall NAME` where `NAME` is the crates.io package name.

Package versions and targets may be specified using the `--version` and `--target` arguments respectively, and will be installed into `$HOME/.cargo/bin` by default. For additional options please see `cargo binstall --help`.

```
[garry] ➜  ~ cargo binstall radio-sx128x --version 0.14.1-alpha.5
21:14:09 [INFO] Installing package: 'radio-sx128x'
21:14:13 [INFO] Downloading package from: 'https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-apple-darwin.tgz'
21:14:18 [INFO] This will install the following binaries:
21:14:18 [INFO]   - sx128x-util (sx128x-util-x86_64-apple-darwin -> /Users/ryankurte/.cargo/bin/sx128x-util-v0.14.1-alpha.5)
21:14:18 [INFO] And create (or update) the following symlinks:
21:14:18 [INFO]   - sx128x-util (/Users/ryankurte/.cargo/bin/sx128x-util-v0.14.1-alpha.5 -> /Users/ryankurte/.cargo/bin/sx128x-util)
21:14:18 [INFO] Do you wish to continue? yes/no
yes
21:15:30 [INFO] Installing binaries...
21:15:30 [INFO] Installation complete!
```

### Unsupported crates

To install an unsupported crate, you may specify the Cargo.toml metadata entries for `pkg-url`, `bin-dir`, and `pkg-fmt` at the command line, with values [as documented below](#supporting-binary-installation).

For example:
```
$ binstall \
  --pkg-url="{ repo }/releases/download/{ version }/{ name }-{ version }-{ target }.{ archive-format }" \
  --pkg-fmt="txz" crate_name
```


## FAQ

- Why use this?
  - Because `wget`-ing releases is frustrating, `cargo install` takes a not inconsequential portion of forever on constrained devices,
    and often putting together actual _packages_ is overkill.
- Why use the cargo manifest?
  - Crates already have these, and they already contain a significant portion of the required information.
    Also there's this great and woefully underused (imo) `[package.metadata]` field.
- Is this secure?
  - Yes and also no? We're not (yet? [#1](https://github.com/cargo-bins/cargo-binstall/issues/1)) doing anything to verify the CI binaries are produced by the right person / organisation.
    However, we're pulling data from crates.io and the cargo manifest, both of which are _already_ trusted entities, and this is
    functionally a replacement for `curl ... | bash` or `wget`-ing the same files, so, things can be improved but it's also fairly moot
- What do the error codes mean?
  - You can find a full description of errors including exit codes here: <https://docs.rs/cargo-binstall/latest/cargo_binstall/errors/enum.BinstallError.html>

---

If you have ideas / contributions or anything is not working the way you expect (in which case, please include an output with `--log-level debug`) and feel free to open an issue or PR.
