# Cargo B(inary)Install

`cargo binstall` provides a low-complexity mechanism for installing rust binaries as an alternative to building from source (via `cargo install`) or manually downloading packages. This is intended to work with existing CI artifacts and infrastructure, and with minimal overhead for package maintainers.

`binstall` works by fetching the crate information from `crates.io`, then searching the linked `repository` for matching releases and artifacts, with fallbacks to [quickinstall](https://github.com/alsuren/cargo-quickinstall) and finally `cargo install` if these are not found.
To support `binstall` maintainers must add configuration values to `Cargo.toml` to allow the tool to locate the appropriate binary package for a given version and target. See [SUPPORT.md](./SUPPORT.md) for more detail.

## Status

![Build](https://github.com/cargo-bins/cargo-binstall/workflows/Rust/badge.svg)
[![GitHub tag](https://img.shields.io/github/tag/cargo-bins/cargo-binstall.svg)](https://github.com/cargo-bins/cargo-binstall)
[![Crates.io](https://img.shields.io/crates/v/cargo-binstall.svg)](https://crates.io/crates/cargo-binstall)

You probably want to **[see this page as it was when the latest version was published](https://crates.io/crates/cargo-binstall)** for accurate documentation.

## Installation

To get started _using_ `cargo-binstall` first install the binary (either via `cargo install cargo-binstall` or by downloading a pre-compiled [release](https://github.com/cargo-bins/cargo-binstall/releases)), then extract it using `tar` or `unzip` and move it into `$HOME/.cargo/bin`.
We recommend using the pre-compiled ones because we optimize those more than a standard source build does.

| OS      | Arch    | URL                                                          |
| ------- | ------- | ------------------------------------------------------------ |
| linux   | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-unknown-linux-musl.tgz |
| linux   | armv7   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-unknown-linux-musl.tgz |
| linux   | arm64   | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-unknown-linux-musl.tgz |
| macos   | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-apple-darwin.zip |
| macos   | m1      | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-apple-darwin.zip |
| macos   | universal | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-universal-apple-darwin.zip |
| windows | x86\_64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-pc-windows-msvc.zip |
| windows | arm64 | https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-aarch64-pc-windows-msvc.zip |

We also provide pre-built artifacts with debuginfo for Linux and Mac.
These artifacts is postfixed with `.full.tgz` on Linux and `.full.zip` on Mac.

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

If your favorite package fails to install, you may specify the Cargo.toml metadata entries for `pkg-url`, `bin-dir`, and `pkg-fmt` at the command line, with values [as documented below](#supporting-binary-installation).

For example:

```shell
$ binstall \
  --pkg-url="{ repo }/releases/download/{ version }/{ name }-{ version }-{ target }.{ archive-format }" \
  --pkg-fmt="txz" crate_name
```

## Upgrade installed crates

The most ergonomic way to upgrade the installed crates is with [`cargo-update`](https://github.com/nabijaczleweli/cargo-update). `cargo-update` automatically uses `cargo-binstall` to install the updates if `cargo-binstall` is present.

Supported crates such as `cargo-binstall` itself can also be updated with `cargo-binstall` as in the example in [Installation](#installation) above.

## FAQ

- Why use this?
  - Because `wget`-ing releases is frustrating, `cargo install` takes a not inconsequential portion of forever on constrained devices,
    and often putting together actual _packages_ is overkill.
- Why use the cargo manifest?
  - Crates already have these, and they already contain a significant portion of the required information.
    Also, there's this great and woefully underused (IMO) `[package.metadata]` field.
- Is this secure?
  - Yes and also no? We're not (yet? [#1](https://github.com/cargo-bins/cargo-binstall/issues/1)) doing anything to verify the CI binaries are produced by the right person/organization.
    However, we're pulling data from crates.io and the cargo manifest, both of which are _already_ trusted entities, and this is
    functionally a replacement for `curl ... | bash` or `wget`-ing the same files, so, things can be improved but it's also fairly moot
- What do the error codes mean?
  - You can find a full description of errors including exit codes here: <https://docs.rs/binstalk/latest/binstalk/errors/enum.BinstallError.html>
- Can I use it in CI?
  - Yes! For GitHub Actions, we recommend the excellent [taiki-e/install-action](https://github.com/marketplace/actions/install-development-tools), which has explicit support for selected tools and uses `cargo-binstall` for everything else.
- Are debug symbols available?
  - Yes! Extra pre-built packages with a `.full` suffix are available and contain split debuginfo, documentation files, and extra binaries like the `detect-wasi` utility.

---

If you have ideas/contributions or anything is not working the way you expect (in which case, please include an output with `--log-level debug`) and feel free to open an issue or PR.
