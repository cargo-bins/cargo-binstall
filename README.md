# Cargo B(inary)Install

`cargo binstall` provides a low-complexity mechanism for installing rust binaries as an alternative to building from source (via `cargo install`) or manually downloading packages. This is intended to work with existing CI artifacts and infrastructure, and with minimal overhead for package maintainers.
To support `binstall` maintainers must add configuration values to `Cargo.toml` to allow the tool to locate the appropriate binary package for a given version and target. See [Supporting Binary Installation](#Supporting-Binary-Installation) for instructions on how to support `binstall` in your projects.

## Installing

To get started _using_ `cargo-binstall`, first install the binary (either via `cargo install cargo-binstall` or by downloading a pre-compiled [release](https://github.com/ryankurte/cargo-binstall/releases).

linux x86_64:
```
wget https://github.com/ryankurte/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-unknown-linux-gnu.tgz
```

linux armv7:
```
wget https://github.com/ryankurte/cargo-binstall/releases/latest/download/cargo-binstall-armv7-unknown-linux-gnueabihf.tgz
```

macos x86_64:
```
wget https://github.com/ryankurte/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-apple-darwin.zip
```

windows x86_64:
```
wget https://github.com/ryankurte/cargo-binstall/releases/latest/download/cargo-binstall-x86_64-pc-windows-msvc.zip
```

## Usage

Supported packages can be installed using `cargo binstall NAME` where `NAME` is the crate.io package name.

Package versions and targets may be specified using the `--version` and `--target` arguments respectively, and install directory with `--install-dir` (this defaults to `$HOME/.cargo/bin`, with fall-backs to `$HOME/.bin` if unavailable). For additional options please see `cargo binstall --help`.

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


## Status

![Build](https://github.com/ryankurte/cargo-binstall/workflows/Rust/badge.svg)
[![GitHub tag](https://img.shields.io/github/tag/ryankurte/cargo-binstall.svg)](https://github.com/ryankurte/cargo-binstall)
[![Crates.io](https://img.shields.io/crates/v/cargo-binstall.svg)](https://crates.io/crates/cargo-binstall)
[![Docs.rs](https://docs.rs/cargo-binstall/badge.svg)](https://docs.rs/cargo-binstall)

### Features

- Manifest discovery
  - [x] Fetch crate / manifest via crates.io
  - [ ] Fetch crate / manifest via git (/ github / gitlab)
  - [x] Use local crate / manifest (`--manifest-path`)
  - [ ] Unofficial packaging
- Package formats
  - [x] Tgz
  - [x] TarGz
  - [x] Txz
  - [x] Tar
  - [x] Zip
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

To get started, add a `[package.metadata.binstall]` section to your `Cargo.toml`. As an example, the default configuration would be:

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

`pkg-url`, `pkg-fmt` and `bin-path` can be overridden on a per-target basis if required, for example, if your `x86_64-pc-windows-msvc` builds use `zip` archives this could be set via:

```toml
[package.metadata.binstall.overrides.x86_64-pc-windows-msvc]
pkg-fmt = "zip"
```

### Defaults

By default `binstall` is setup to work with github releases, and expects to find:

- an archive named `{ name }-{ target }-v{ version }.{ format }`
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

## Installing unsupported crates

If you would like to use this tool to download crates unsupported crates, you can override the following templates at the command line:

- `pkg-url` templated specification for the package download URL for a given target/version
- `bin-path` templated specification for the binary path within the package (`.exe` suffix added automatically on windows)
- `pkg-fmt` overrides the package format for download/extraction (defaults to: `tgz`)

These values replace the template and are applied over any metadata supplied by the package. Once a package adds support, removing these overrides is best practice.

### Examples

This crate has no v on the version number, the version and target are switched, and uses tar.gz extension.
```
[garry] ➜  ~  binstall --pkg-url="{ repo }/releases/download/{ version }/{ name }-{ version }-{ target }.{ format }" --pkg-fmt="tar.gz" crate_name
```

## FAQ

- Why use this?
  - Because `wget`-ing releases is frustrating, `cargo install` takes a not inconsequential portion of forever on constrained devices,
    and often putting together actual _packages_ is overkill.
- Why use the cargo manifest?
  - Crates already have these, and they already contain a significant portion of the required information.
    Also there's this great and woefully underused (imo) `[package.metadata]` field.
- Why not use a binary repository instead?
  - Then we'd need to _host_ a binary repository, and worry about publishing and all the other fun things that come with releasing software.
    This way we can use existing CI infrastructure and build artifacts, and maintainers can choose how to distribute their packages.
- Is this secure?
  - Yes and also no? We're not (yet? #1) doing anything to verify the CI binaries are produced by the right person / organisation.
    However, we're pulling data from crates.io and the cargo manifest, both of which are _already_ trusted entities, and this is
    functionally a replacement for `curl ... | bash` or `wget`-ing the same files, so, things can be improved but it's also sorta moot

---

If you have ideas / contributions or anything is not working the way you expect (in which case, please include an output with `--log-level debug`) and feel free to open an issue or PR.
