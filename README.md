# Cargo B(inary)Install

A helper for distributing / installing CI built rust binaries in a pseudo-distributed and maybe-one-day secure manner.
This tool is not intended to manage the _building_ of rust binaries as CI can already readily manage this, but to provide a simple project-level mechanism for the distribution and consumption of rust binary packages.

To support `binstall` maintainers must add configuration values to `Cargo.toml` to allow the tool to locate the appropriate CI-produced binary package for a given version and target. For further information on adding `binstall` support, see [Supporting Binary Installation](#Supporting-Binary-Installation) for further detail on supporting `binstall`.

For packages with `binstall` support, the command `cargo binstall PACKAGE` will then look-up the crate metadata for the specified (or latest) version, fetch the associated binary package, and install this to `$HOME/.cargo/bin` (or `$HOME/.bin` if this is unavailable). See [Installing Binaries](#Installing-Binaries) for further information on using `binstall`.

Cargo metadata is used to avoid the need for an additional centralised index or binary repository, and to provide project maintainers with the maximum possible agency for binary distribution with no additional dependencies and minimal additional complexity. This is part experiment, part solving a personal problem, and part hope that we can solve / never re-visit this. I hope you find it helpful and, good luck!

## Status

![Rust](https://github.com/ryankurte/cargo-binstall/workflows/Rust/badge.svg)
[![GitHub tag](https://img.shields.io/github/tag/ryankurte/cargo-binstall.svg)](https://github.com/ryankurte/cargo-binstall)
[![Crates.io](https://img.shields.io/crates/v/cargo-binstall.svg)](https://crates.io/crates/cargo-binstall)
[![Docs.rs](https://docs.rs/cargo-binstall/badge.svg)](https://docs.rs/cargo-binstall)

## Features

- Manifest discovery
  - [x] Fetch manifest via crates.io
  - [ ] Fetch manifest via git
  - [x] Use local manifest (`--manifest-path`)
- Package formats
  - [x] Tgz
  - [x] Tar
  - [x] Bin
- Extraction / Transformation
  - [ ] Extract from subdirectory in archive (ie. support archives with platform or target subdirectories)
  - [ ] Extract specific files from archive (ie. support single archive with multiple platform binaries)
- Security
  - [ ] Package signing
  - [ ] Package verification

## Installing Binaries

First you'll need to install `cargo-binstall` either via `cargo install cargo-binstall` (and it'll have to compile, sorry...), or by grabbing a pre-compiled version from the [releases](https://github.com/ryankurte/cargo-binstall/releases) page and putting that somewhere on your path. It's like there's a problem we're trying to solve?

Once a project supports `binstall` you can then install binaries via `cargo binstall NAME` where `NAME` is the name of the crate. This will then fetch the metadata for the provided crate, lookup the associated binary file, and download this onto your system. 
By default the latest version is installed, which can be overridden using the `--version` argument, and packages are installed to `$HOME/.cargo/bin` as is consistent with `cargo install`, which can be overridden via the `--install-path` argument. As always `--help` will show available options.

We hope the defaults will work without configuration in _some_ cases, however, different projects have wildly different CI and build output configurations. You will likely need to add some cargo metadata to support `binstall` in your project, see [Supporting Binary Installation](#Supporting-Binary-Installation) for details.


## Supporting Binary Installation

`cargo-binstall` installs binary packages first by reading `[package.metadata]` values from the Cargo manifest (`Cargo.toml`) to discover a template URL, then by building a download path from this template, and finally by downloading and extracting the binary package onto the users path.

To support `binstall` first you need working CI that places binary outputs at a reasonably deterministic location (github releases, S3 bucket, so long as it's internet accessible you're good to go), then to add configuration values to your Cargo manifest to specify a template string for `binstall` to use when downloading packages.

By default `binstall` will look for pre-built packages using the following template:
```
{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ format }`, 
```

Template variables use the format `{ NAME }` where `NAME` is the name of the variable.
`repo`, `name`, and `version` are those specified in the crate manifest (`Cargo.toml`).
`target` defaults to your (the machine calling `cargo binstall`) architecture, but can be overridden using the `--target` command line option if required.
`format` defaults to `tgz` and can be specified via the `pkg-fmt` key under `[package.metadata]`. You may need this if you have sneaky `tgz` files that are actually not gzipped.

For example, for the `radio-sx128x` crate at version `v0.14.1-alpha.5`, this would be interpolated to a download URL of:
```
https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/rust-radio-sx128x-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz`
```

Custom template URLS may be specified using the `pkg-url` field under `[package.metadata]`, using the same keywords for interpolation as discussed above. Again for the `radio-sx128x` package this could be:

```
[package.metadata]
pkg-url = "https://github.com/ryankurte/rust-radio-sx128x/releases/download/v{ version }/radio-sx128x-{ target }-v{ version }.tgz"
```

If you have separate crate and package names, you can specify a `pkg-name` under `[package.metadata]`, replacing _only_ the `{ name }` field in the default template.
This is useful if you have a library crate with utilities, and the crate and binary package names differ, but can equally well be addressed via defining the `pkg-url` field as described above.
For example, the real-world `ryankurte/radio-sx128x` crate produces a `sx128x-util` package, which can be configured using the following:

```
[package.metadata]
pkg-name = "sx128x-util"
```

Once you've added a `pkg-name` or a `pkg-url` you should be good to go! For the purposes of testing this integration you may find it useful to manually specify the manifest path using the `--manifest-path=PATH` argument, this skips discovery of the crate manifest and uses the one at the provided `PATH`. For testing purposes you can also override a variety of configuration options on the command line, see `--help` for more details.

---

If anything is not working the way you expect, add a `--log-level debug` to see debug information, and feel free to open an issue or PR.
