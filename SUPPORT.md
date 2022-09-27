# Support for `cargo binstall`


`binstall` works with existing CI-built binary outputs, with configuration via `[package.metadata.binstall]` keys in the relevant crate manifest.
When configuring `binstall` you can test against a local manifest with `--manifest-path=PATH` argument to use the crate and manifest at the provided `PATH`, skipping crate discovery and download.

To get started, check the [default](#Defaults) first, only add a `[package.metadata.binstall]` section
to your `Cargo.toml` if the default does not work for you.

As an example, the configuration would be like this:

```toml
[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/{ name }-{ target }-v{ version }{ archive-suffix }"
bin-dir = "{ name }-{ target }-v{ version }/{ bin }{ binary-ext }"
pkg-fmt = "tgz"
```

With the following configuration keys:

- `pkg-url` specifies the package download URL for a given target/version, templated
- `bin-dir` specifies the binary path within the package, templated (with an `.exe` suffix on windows)
- `pkg-fmt` overrides the package format for download/extraction (defaults to: `tgz`)


`pkg-url` and `bin-dir` are templated to support different names for different versions / architectures / etc.
Template variables use the format `{ VAR }` where `VAR` is the name of the variable, with the following variables available:
- `name` is the name of the crate/package
- `version` is the crate version (per `--version` and the crate manifest)
- `repo` is the repository linked in `Cargo.toml`
- `bin` is the name of a specific binary, inferred from the crate configuration
- `target` is the rust target name (defaults to your architecture, but can be overridden using the `--target` command line option if required()
- `archive-suffix` is the filename extension of the package archive format, e.g. `.tar.tgz` for tgz or `.exe`/`""` for bin.
- `archive-format` is the filename extension of the package archive format, e.g. `tar.tgz` for tgz or `exe`/`""` for bin.
- `binary-ext` is the string `.exe` if the `target` is for Windows, or the empty string otherwise
- `format` is a soft-deprecated alias for `archive-format` in `pkg-url`, and alias for `binary-ext` in `bin-dir`; in the future, this may warn at install time.

`pkg-url`, `pkg-fmt` and `bin-dir` can be overridden on a per-target basis if required, for example, if your `x86_64-pc-windows-msvc` builds use `zip` archives this could be set via:

```
[package.metadata.binstall.overrides.x86_64-pc-windows-msvc]
pkg-fmt = "zip"
```

### Defaults

By default, `binstall` will try all supported package formats and would have `bin-dir` set to
`"{ name }-{ target }-v{ version }/{ bin }{ binary-ext }"` (where `bin` is the cargo binary name and
`binary-ext` is `.exe` on windows and empty on other platforms).

All binaries must contain a folder named `{ name }-{ target }-v{ version }` (so that prior binary
files are not overwritten when manually executing `tar -xvf ...`).

The default value for `pkg-url` will depend on the repository of the package.

It is set up to work with GitHub releases, GitLab releases, bitbucket downloads
and source forge downloads.

If your package already uses any of these URLs, you shouldn't need to set anything.

The URLs are derived from a set of filenames and a set of paths, which are
"multiplied together": every filename appended to every path. The filenames
are:

- `{ name }-{ target }-{ version }{ archive-suffix }`
- `{ name }-{ target }-v{ version }{ archive-suffix }`
- `{ name }-{ version }-{ target }{ archive-suffix }`
- `{ name }-v{ version }-{ target }{ archive-suffix }`
- `{ name }-{ version }-{ target }{ archive-suffix }`
- `{ name }-v{ version }-{ target }{ archive-suffix }`
- `{ name }-{ target }{ archive-suffix }` ("versionless")

The paths are:

#### for GitHub

- `{ repo }/releases/download/{ version }/`
- `{ repo }/releases/download/v{ version }/`

#### for GitLab

- `{ repo }/-/releases/{ version }/downloads/binaries/`
- `{ repo }/-/releases/v{ version }/downloads/binaries/`

Note that this uses the [Permanent links to release assets][gitlab-permalinks]
feature of GitLab EE: it requires you to create an asset as a link with a
`filepath`, which, as of writing, can only be set using GitLab's API.

[gitlab-permalinks]: https://docs.gitlab.com/ee/user/project/releases/index.html#permanent-links-to-latest-release-assets

#### for BitBucket

- `{ repo }/downloads/`

Binaries must be uploaded to the project's "Downloads" page on BitBucket.

Also note that as there are no per-release downloads, the "versionless"
filename is not considered here.

#### for SourceForge

- `{ repo }/files/binaries/{ version }`
- `{ repo }/files/binaries/v{ version }`

The URLs also have `/download` appended as per SourceForge's schema.

Binary must be uploaded to the "File" page of your project, under the directory
`binaries/v{ version }`.

#### Others

For all other situations, `binstall` does not provide a default `pkg-url` and
you need to manually specify it.

### QuickInstall

[QuickInstall](https://github.com/alsuren/cargo-quickinstall) is an unofficial repository of prebuilt binaries for Crates, and `binstall` has built-in support for it! If your crate is built by QuickInstall, it will already work with `binstall`. However, binaries as configured above take precedence when they exist.

### Examples

For example, the default configuration (as shown above) for a crate called `radio-sx128x` (version: `v0.14.1-alpha.5` on x86\_64 linux) would be interpolated to:

- A download URL of `https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/rust-radio-sx128x-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz`
- Containing a single binary file `rust-radio-sx128x-x86_64-unknown-linux-gnu-v0.14.1-alpha.5/rust-radio-x86_64-unknown-linux-gnu`
- Installed to`$HOME/.cargo/bin/rust-radio-sx128x-v0.14.1-alpha.5`
- With a symlink from `$HOME/.cargo/bin/rust-radio-sx128x`

####  If the package name does not match the crate name

As is common with libraries/utilities (and the `radio-sx128x` example), this can be overridden by specifying the `pkg-url`:

```toml
[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/sx128x-util-{ target }-v{ version }{ archive-suffix }"
```

Which provides a download URL of `https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/sx128x-util-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz`


####  If the package structure differs from the default

Were the package to contain binaries in the form `name-target[.exe]`, this could be overridden using the `bin-dir` key:

```toml
[package.metadata.binstall]
bin-dir = "{ bin }-{ target }{ binary-ext }"
```

Which provides a binary path of: `sx128x-util-x86_64-unknown-linux-gnu[.exe]`. It is worth noting that binary names are inferred from the crate, so long as cargo builds them this _should_ just work.
