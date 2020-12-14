# Cargo B(inary) Install

A helper for distributing / installing pre-built rust binaries in a pseudo-distributed and maybe-one-day secure manner. 
This is part experiment, part solving a personal problem, and part hope that we can solve / never re-visit this. Good luck!

## Status

![Rust](https://github.com/ryankurte/cargo-binstall/workflows/Rust/badge.svg)
[![GitHub tag](https://img.shields.io/github/tag/ryankurte/cargo-binstall.svg)](https://github.com/ryankurte/cargo-binstall)
[![Crates.io](https://img.shields.io/crates/v/cargo-binstall.svg)](https://crates.io/crates/cargo-binstall)
[![Docs.rs](https://docs.rs/cargo-binstall/badge.svg)](https://docs.rs/cargo-binstall)


## Getting Started

First you'll need to install `cargo-binstall` either via `cargo install cargo-binstall` (and it'll have to compile, sorry...), or by grabbing a pre-compiled version from the [releases](https://github.com/ryankurte/cargo-binstall/releases) page and putting that on your path. It's like there's a problem we're trying to solve?

If a project supports `binstall` you can then install binaries via `cargo binstall NAME` where `NAME` is the name of the crate. We hope the defaults will work without configuration in _some_ cases, however, different projects have wildly different configurations. You may need to add some cargo metadata to support `binstall` in your project, see [Usage](#Usage) for details.


## Features

- Manifest discovery
  - [x] Fetch manifest from crates.io
  - [ ] Fetch manifest using git
  - [x] Use local manifest (`--manifest-path`)
- Package formats
  - [x] Tgz
  - [x] Tar
  - [x] Bin
- Security
  - [ ] Package signing
  - [ ] Package verification

## Usage

Packages are located first by locating or querying for a manifest (to allow configuration of the tool), then by interpolating a templated string to download the required package. Where possible defaults are provided to avoid any need for additional configuration, these can generally be overridden via `[package.metadata]` keys at a project level, or on the command line as required (and for debugging), see `cargo binstall -- help` for details.


By default `binstall` will look for pre-built packages at `{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }.{ format }`, where `repo`, `name`, and `version` are those specified in the crate manifest (`Cargo.toml`).
`target` defaults to your architecture, but can be overridden using the `--target` command line option _if required_, and `format` defaults to `tgz` and can be specified via the `pkg-fmt` key (you may need this if you have sneaky `tgz` files that are actually not gzipped).

To support projects with different binary URLs you can override these via the following mechanisms:

To replace _only_ the the package name, specify (`pkg-name`) under `[package.metadata]`. This is useful if you're using github, and your binary paths mostly match except that output package names differ from your crate name. As an example, the `ryankurte/radio-sx128x` crate produces a `sx128x-util` package, and can be configured using the following:

```
[package.metadata]
pkg-name = "sx128x-util"
```

To replace the entire URL, with all the benefits of interpolation, specify (`pkg-url`) under `[package.metadata]`.
This lets you customise the URL for completely different paths (or different services!). Using the same example as above, this becomes:

```
[package.metadata]
pkg-url = "https://github.com/ryankurte/rust-radio-sx128x/releases/download/v{ version }/sx128x-util-{ target }-v{ version }.tgz"
```

---

If anything is not working the way you expect, add a `--log-level debug` to see debug information, and feel free to open an issue or PR.
