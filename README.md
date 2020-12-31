# Cargo B(inary)Install

A helper for distribution and installation of CI built rust binaries in a pseudo-distributed and maybe-one-day secure manner. This is part experiment, part solving a personal problem, and part hope that we can solve / never re-visit this. I hope you find it helpful and, good luck!

To get started _using_ `cargo-binstall`, first install the binary (either via `cargo install cargo-binstall` or by downloading a pre-compiled [release](https://github.com/ryankurte/cargo-binstall/releases). Once you have installed this you can of course `cargo binstall cargo-binstall` to install further versions...

Supported packages can be installed using `cargo binstall NAME` where `NAME` is the crate.io package name.
Package versions and targets may be specified using the `--version` and `--target` arguments respectively, and install directory with `--install-dir` (this defaults to `$HOME/.cargo/bin`, with fall-backs to `$HOME/.bin` if unavailable). For additional options please see `cargo binstall --help`.

To support `binstall` maintainers must add configuration values to `Cargo.toml` to allow the tool to locate the appropriate CI-produced binary package for a given version and target. See [Supporting Binary Installation](#Supporting-Binary-Installation) for instructions on how to support `binstall` in your projects.

## Status

![Build](https://github.com/ryankurte/cargo-binstall/workflows/Rust/badge.svg)
[![GitHub tag](https://img.shields.io/github/tag/ryankurte/cargo-binstall.svg)](https://github.com/ryankurte/cargo-binstall)
[![Crates.io](https://img.shields.io/crates/v/cargo-binstall.svg)](https://crates.io/crates/cargo-binstall)
[![Docs.rs](https://docs.rs/cargo-binstall/badge.svg)](https://docs.rs/cargo-binstall)

## Features

- Manifest discovery
  - [x] Fetch crate / manifest via crates.io
  - [ ] Fetch crate / manifest via git
  - [x] Use local crate / manifest (`--manifest-path`)
- Package formats
  - [x] Tgz
  - [x] Tar
  - [x] Bin
- Extraction / Transformation
  - [x] Extract from subdirectory in archive (ie. support archives with platform or target subdirectories)
  - [x] Extract specific files from archive (ie. support single archive with multiple platform binaries)
- Security
  - [ ] Package signing
  - [ ] Package verification

## Supporting Binary Installation

`binstall` works with existing CI-built binary outputs, with configuration via `[package.metadata.binstall]` keys in the relevant crate manifest. 
When configuring `binstall` you can test against a local manifest with `--manifest-path=PATH` argument to use the crate and manifest at the provided `PATH`, skipping crate discovery and download.

To get started, add a `[package.metadata.binstall]` section to your `Cargo.toml. As an example, the default configuration would be:

```toml
[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ format }"
bin-dir = "{ name }-{ target }-v{ version }/{ bin }{ format }"
pkg-fmt = "tgz"
```

With the following configuration keys:

- `pkg-url` specifies the package download URL for a given target/version, templated
- `bin-path` specifies the binary path within the package, templated (with an `.exe` suffix on windows)
- `pkg-fmt` overrides the package format for download/extraction (defaults to: `tgz`)


`pkg-url` and `bin-path` are templated to support different names for different versions / architectures / etc.
Template variables use the format `{ VAR }` where `VAR` is the name of the variable, with the following variables available:
- `name` is the name of the crate / package
- `version` is the crate version (per `--version` and the crate manifest)
- `repo` is the repository linked in `Cargo.toml`
- `bin` is the name of a specific binary, inferred from the crate configuration
- `target` is the rust target name (defaults to your architecture, but can be overridden using the `--target` command line option if required().


### Defaults

By default `binstall` is setup to work with github releases, and expects to find:

- an archive named `{ name }-{ target }-v{ version }.tgz`
  - so that this does not overwrite different targets or versions when manually downloaded
- located at `{ repo }/releases/download/v{ version }/`
  - compatible with github tags / releases
- containing a folder named `{ name }-{ target }-v{ version }`
  - so that prior binary files are not overwritten when manually executing `tar -xvf ...`
- containing binary files in the form `{ bin }{ format }` (where `bin` is the cargo binary name and `format` is `.exe` on windows and empty on other platforms)

If your package already uses this approach, you shouldn't need to set anything.

### Examples

For example, the default configuration (as shown above) for a crate called `radio-sx128x` (version: `v0.14.1-alpha.5` on x86_64 linux) would be interpolated to:

- A download URL of `https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/rust-radio-sx128x-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz`
- Containing a single binary file `rust-radio-sx128x-x86_64-unknown-linux-gnu-v0.14.1-alpha.5/rust-radio-x86_64-unknown-linux-gnu`
- Installed to`$HOME/.cargo/bin/rust-radio-sx128x-v0.14.1-alpha.5`
- With a symlink from `$HOME/.cargo/bin/rust-radio-sx128x`

####  If the package name does not match the crate name 

As is common with libraries / utilities (and the `radio-sx128x` example), this can be overridden by specifying the `pkg-url`:

```toml
[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.{ format }"
```

Which provides a download URL of: `https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz`


####  If the package structure differs from the default

Were the package to contain binaries in the form `name-target[.exe]`, this could be overridden using the `bin-dir` key:

```toml
[package.metadata.binstall]
bin-dir = "{ bin }-{ target }{ format }"
```

Which provides a binary path of: `sx128x-util-x86_64-unknown-linux-gnu[.exe]`. It is worth noting that binary names are inferred from the crate, so long as cargo builds them this _should_ just work.


---

If you have ideas / contributions or anything is not working the way you expect (in which case, please include an output with `--log-level debug`) and feel free to open an issue or PR.
