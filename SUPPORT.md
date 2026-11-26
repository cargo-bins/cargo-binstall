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
disabled-strategies = ["quick-install", "compile"]
```

With the following configuration keys:

- `pkg-url` specifies the package download URL for a given target/version, templated
- `bin-dir` specifies the binary path within the package, templated (with an `.exe` suffix on windows).
  It may also be the dir holding the binaries, see [If the package structure differs from the default](#if-the-package-structure-differs-from-the-default).
  Despite the name, it is a path inside of the package, not where the binaries are installed (that is `--install-path`).
- `pkg-fmt` overrides the package format for download/extraction (defaults to: `tgz`), check [the documentation](https://docs.rs/binstalk-types/latest/binstalk_types/cargo_toml_binstall/enum.PkgFmt.html) for all supported formats.
  `bin` is for a download that is the binary itself, and `gz`, `bz2`, `xz` and `zst` for a binary compressed on its own, without a TAR around it (e.g. `{ name }-{ target }.gz`).
  These hold a single file with no name inside, so it is installed as the binary named in `Cargo.toml` and `bin-dir` is not used.
- `disabled-strategies` to disable specific strategies (e.g. `crate-meta-data` for trying to find pre-built on your repository,
  `quick-install` for pre-built from third-party cargo-bins/cargo-quickinstall, `compile` for falling back to `cargo-install`)
  for your crate (defaults to empty array).
  If `--strategies` is passed on the command line, then the `disabled-strategies` in `package.metadata` will be ignored.
  Otherwise, the `disabled-strategies` in `package.metadata` and `--disable-strategies` will be merged.
- `binary.NAME.aliases` is a list of extra names to symlink to the installed binary `NAME`, see [Aliases](#aliases).


`pkg-url` and `bin-dir` are templated to support different names for different versions / architectures / etc.
Template variables use the format `{ VAR }` where `VAR` is the name of the variable,
`\{` for literal `{`, `\}` for literal `}` and `\\` for literal `\`,
with the following variables available:
- `name` is the name of the crate/package
- `version` is the crate version (per `--version` and the crate manifest)
- `repo` is the repository linked in `Cargo.toml`
- `subcrate` is the crate's directory when `repo` points at a crate inside a repository, see [Defaults](#defaults); templates using it are skipped otherwise
- `bin` is the name of a specific binary, inferred from the crate configuration.
  In `pkg-url`, it makes each binary be downloaded from its own package, see [One package per binary](#one-package-per-binary)
- `target` is the rust target name (defaults to your architecture, but can be overridden using the `--target` command line option if required)
- `archive-suffix` is the filename extension of the package archive format that includes the prefix `.`, e.g. `.tgz` for tgz or `.exe`/`""` for bin.
- `archive-format` is the soft-deprecated filename extension of the package archive format that does not include the prefix `.`, e.g. `tgz` for tgz or `exe`/`""` for bin.
- `binary-ext` is the string `.exe` if the `target` is for Windows, or the empty string otherwise
- `format` is a soft-deprecated alias for `archive-format` in `pkg-url`, and alias for `binary-ext` in `bin-dir`; in the future, this may warn at install time.
- `target-family`: Operating system of the target from [`target_lexicon::OperatingSystem`]
- `os-name`: `rustc`-style operating system name of the target, e.g. `windows`, `macos` or `linux`
- `target-arch`: Architecture of the target, `universal` on `{universal, universal2}-apple-darwin`,
  otherwise from [`target_lexicon::Architecture`].
  When `pkg-url` uses it, binstall also tries the common aliases `arm64` for `aarch64` and `amd64` for `x86_64`,
  after Rust's name, and the alias that is found is also used for `bin-dir` and the signature `file`
- `target-libc`: ABI environment of the target from [`target_lexicon::Environment`]
- `target-vendor`: Vendor of the target from [`target_lexicon::Vendor`]

[`target_lexicon::OperatingSystem`]: https://docs.rs/target-lexicon/latest/target_lexicon/enum.OperatingSystem.html
[`target_lexicon::Architecture`]: https://docs.rs/target-lexicon/latest/target_lexicon/enum.Architecture.html
[`target_lexicon::Environment`]: https://docs.rs/target-lexicon/latest/target_lexicon/enum.Environment.html
[`target_lexicon::Vendor`]: https://docs.rs/target-lexicon/latest/target_lexicon/enum.Vendor.html

`pkg-url`, `pkg-fmt` and `bin-dir` can be overridden on a per-target basis if required, for example, if your `x86_64-pc-windows-msvc` builds use `zip` archives this could be set via:

```toml
[package.metadata.binstall.overrides.x86_64-pc-windows-msvc]
pkg-fmt = "zip"
```

#### Using `cfg` expressions

In addition to exact target names, you can use Cargo-style `cfg(...)` expressions to match multiple targets at once. This avoids having to list each target individually:

```toml
# Apply to all Linux targets.
[package.metadata.binstall.overrides.'cfg(target_os = "linux")']
pkg-fmt = "tgz"

# Apply to all Windows targets.
[package.metadata.binstall.overrides.'cfg(target_os = "windows")']
pkg-fmt = "zip"

# Apply to all Unix-like systems.
[package.metadata.binstall.overrides.'cfg(unix)']
bin-dir = "{ bin }"
```

The following `cfg` predicates are available:

- `target_os`: Operating system (e.g., `"linux"`, `"windows"`, `"macos"`)
- `target_arch`: Architecture (e.g., `"x86_64"`, `"aarch64"`, `"universal"`)
- `target_env`: ABI environment (e.g., `"gnu"`, `"msvc"`, `"musl"`)
- `target_vendor`: Vendor (e.g., `"unknown"`, `"apple"`, `"pc"`)
- `target_family`: Operating system family (`"unix"` or `"windows"`)

You can also use:

- `cfg(unix)` as shorthand for Unix-like systems
- `cfg(windows)` as shorthand for Windows

Finally, you can combine predicates using `all()`, `any()`, and `not()`:

```toml
# Match ARM Linux with the GNU C Library.
[package.metadata.binstall.overrides.'cfg(all(target_os = "linux", target_arch = "aarch64", target_env = "gnu"))']
pkg-url = "{ repo }/releases/download/v{ version }/{ name }-arm64-linux{ archive-suffix }"

# Match any non-Windows system.
[package.metadata.binstall.overrides.'cfg(not(target_os = "windows"))']
pkg-fmt = "tgz"
```

**Precedence:** Exact target names take precedence over `cfg` expressions. When multiple `cfg` expressions match, they're evaluated in the order they appear in `Cargo.toml`.


### Aliases

A binary can be given extra names, each installed as a symlink next to it:

```toml
[package.metadata.binstall.binary.my-crate-cli]
aliases = ["mcc"]
```

Each key under `binary` is the name of one of the crate's binaries, so a crate with several binaries can give each its own aliases.
An alias must be a plain file name and it gets the same `.exe` suffix as the binary on Windows.
It is an error for an alias to have the name of a file installed for one of the crate's binaries, or of another alias, and for `binary.NAME` to name a binary the crate doesn't have.
On Windows, a copy of the binary is made if the symlink can't be created.
Aliases are removed on uninstall like the binary itself.
They are only created for prebuilt binaries, not when falling back to installing from source with `cargo install`, which warns about it. With `--aliases` on the command line, that fallback is an error instead.

An entry under `overrides.TARGET.binary` replaces the entry for the same binary, and `--aliases BIN:ALIAS` on the command line replaces both for each `BIN` it names.

### One package per binary

If each binary of the crate is released in its own package, use `{ bin }` in `pkg-url`:

```toml
[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/{ bin }-{ target }{ archive-suffix }"
```

`binstall` then downloads one package per binary (only the ones passed to `--bin`, if any), each from `pkg-url` rendered with that binary's name.
Each package only has to provide its own binary, and any `pkg-fmt` works:

- with `bin`, or a single binary compressed without an archive (`gz`, `bz2`, `xz` or `zst`), the downloaded file is the binary and `bin-dir` is not used;
- with an archive, the binary is found in it with `bin-dir`, inferred as described in [Defaults](#defaults) if not set.

`signing.file` is rendered for each package, so the default `{ url }.sig` gives each package its own signature.

All the packages must be found for the same target, except those of binaries with `required-features`, which are skipped with a warning if missing.

### Defaults

By default, `binstall` will try all supported package formats and would do the same for `bin-dir`.

It will first extract the archives, then iterate over the following list, finding the first dir
that exists:

<!-- BEGIN generated:default-bin-dirs from crates/binstalk-bins/src/lib.rs, run `just update-docs` to update -->
- `{ name }-{ target }-v{ version }`
- `{ name }-{ target }-{ version }`
- `{ name }-{ version }-{ target }`
- `{ name }-v{ version }-{ target }`
- `{ name }-{ target }`
- `{ name }-{ version }`
- `{ name }-v{ version }`
- `{ name }`
<!-- END generated:default-bin-dirs -->

If none of them exists and the archive holds a single dir at its top, that dir is used.

Then it will concat the dir with `"{ bin }{ binary-ext }"` and use that as the final `bin-dir`.

`name` here is the name of the crate, `bin` is the cargo binary name and `binary-ext` is `.exe`
on windows and empty on other platforms.

The default value for `pkg-url` will depend on the repository of the package.

It is set up to work with GitHub releases, GitLab releases, bitbucket downloads,
source forge downloads and Codeberg releases.

If your package already uses any of these URLs, you shouldn't need to set anything.

The URLs are derived from a set of filenames and a set of paths, which are
"multiplied together": every filename appended to every path. The filenames
are:

<!-- BEGIN generated:pkg-url-filenames from crates/binstalk-fetchers/src/gh_crate_meta/hosting.rs, run `just update-docs` to update -->
- `{ name }-{ target }-v{ version }{ archive-suffix }`
- `{ name }-{ target }-{ version }{ archive-suffix }`
- `{ name }-{ version }-{ target }{ archive-suffix }`
- `{ name }-v{ version }-{ target }{ archive-suffix }`
- `{ name }_{ target }_v{ version }{ archive-suffix }`
- `{ name }_{ target }_{ version }{ archive-suffix }`
- `{ name }_{ version }_{ target }{ archive-suffix }`
- `{ name }_v{ version }_{ target }{ archive-suffix }`
- `{ name }-{ target }{ archive-suffix }` ("versionless")
- `{ name }_{ target }{ archive-suffix }` ("versionless")
<!-- END generated:pkg-url-filenames -->

The paths are listed below. Paths containing `{ subcrate }` are only tried
when the crate's `repository` points at a crate inside a GitHub, GitLab or
Codeberg repository, such as `https://github.com/owner/repo/tree/main/crates/foo`
or `https://github.com/owner/repo/tree/main/foo`. `{ subcrate }` is then the
crate's directory (`foo`) and `{ repo }` is the repository itself
(`https://github.com/owner/repo`). The `%2F` in those paths is a URL-encoded
`/`, which matches release tags such as `foo/v1.2.3`.

#### for GitHub

<!-- BEGIN generated:pkg-url-paths-github from crates/binstalk-fetchers/src/gh_crate_meta/hosting.rs, run `just update-docs` to update -->
- `{ repo }/releases/download/{ version }/`
- `{ repo }/releases/download/v{ version }/`
- `{ repo }/releases/download/{ subcrate }%2F{ version }/`
- `{ repo }/releases/download/{ subcrate }%2Fv{ version }/`
- `{ repo }/releases/download/{ name }-v{ version }/`
- `{ repo }/releases/download/{ name }-{ version }/`
<!-- END generated:pkg-url-paths-github -->

##### Releases made by dist

If a release made by [dist](https://github.com/axodotdev/cargo-dist) has the
`dist-manifest.json` that dist uploads to it, under any of the paths above,
`binstall` takes the archive for the target from it, instead of trying every
filename above. The archive is then also checked against the sha256 checksum
the manifest lists for it.

This is only done when the crate doesn't set `pkg-url`: dist's default archive
names and layout already match the defaults above, so nothing needs to be set
for such a release.

#### for GitLab

<!-- BEGIN generated:pkg-url-paths-gitlab from crates/binstalk-fetchers/src/gh_crate_meta/hosting.rs, run `just update-docs` to update -->
- `{ repo }/-/releases/{ version }/downloads/binaries/`
- `{ repo }/-/releases/v{ version }/downloads/binaries/`
- `{ repo }/-/releases/{ subcrate }%2F{ version }/downloads/binaries/`
- `{ repo }/-/releases/{ subcrate }%2Fv{ version }/downloads/binaries/`
<!-- END generated:pkg-url-paths-gitlab -->

Note that this uses the [Permanent links to release assets][gitlab-permalinks]
feature of GitLab EE: it requires you to create an asset as a link with a
`filepath`, which, as of writing, can only be set using GitLab's API.

[gitlab-permalinks]: https://docs.gitlab.com/ee/user/project/releases/index.html#permanent-links-to-latest-release-assets

#### for BitBucket

<!-- BEGIN generated:pkg-url-paths-bitbucket from crates/binstalk-fetchers/src/gh_crate_meta/hosting.rs, run `just update-docs` to update -->
- `{ repo }/downloads/`
<!-- END generated:pkg-url-paths-bitbucket -->

Binaries must be uploaded to the project's "Downloads" page on BitBucket.

Also note that as there are no per-release downloads, the "versionless"
filename is not considered here.

#### for SourceForge

<!-- BEGIN generated:pkg-url-paths-sourceforge from crates/binstalk-fetchers/src/gh_crate_meta/hosting.rs, run `just update-docs` to update -->
- `{ repo }/files/binaries/{ version }/`
- `{ repo }/files/binaries/v{ version }/`
<!-- END generated:pkg-url-paths-sourceforge -->

The URLs also have `/download` appended as per SourceForge's schema.

Binary must be uploaded to the "File" page of your project, under the directory
`binaries/v{ version }`.

#### for Codeberg

<!-- BEGIN generated:pkg-url-paths-codeberg from crates/binstalk-fetchers/src/gh_crate_meta/hosting.rs, run `just update-docs` to update -->
- `{ repo }/releases/download/{ version }/`
- `{ repo }/releases/download/v{ version }/`
- `{ repo }/releases/download/{ subcrate }%2F{ version }/`
- `{ repo }/releases/download/{ subcrate }%2Fv{ version }/`
- `{ repo }/releases/download/{ name }-v{ version }/`
- `{ repo }/releases/download/{ name }-{ version }/`
<!-- END generated:pkg-url-paths-codeberg -->

#### Others

For all other situations, `binstall` does not provide a default `pkg-url` and
you need to manually specify it.

### QuickInstall

[QuickInstall](https://github.com/alsuren/cargo-quickinstall) is an unofficial repository of prebuilt binaries for Crates, and `binstall` has built-in support for it! If your crate is built by QuickInstall, it will already work with `binstall`. However, binaries as configured above take precedence when they exist.

### Examples

For example, the default configuration (as shown above) for a crate called `radio-sx128x` (version: `v0.14.1-alpha.5` on x86\_64 linux) would be interpolated to:

- A download URL of `https://github.com/rust-iot/rust-radio-sx128x/releases/download/v0.14.1-alpha.5/rust-radio-sx128x-x86_64-unknown-linux-gnu-v0.14.1-alpha.5.tgz`
- Containing a single binary file `rust-radio-sx128x-x86_64-unknown-linux-gnu-v0.14.1-alpha.5/rust-radio-x86_64-unknown-linux-gnu`
- Installed to `$HOME/.cargo/bin/rust-radio-sx128x-v0.14.1-alpha.5`
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

Which provides a binary path of: `sx128x-util-x86_64-unknown-linux-gnu[.exe]`. It is worth noting that binary names are inferred from the crate, so as long as cargo builds them this _should_ just work.

If `bin-dir` renders to a dir in the package, the binary is looked up in it as `{ bin }{ binary-ext }`, so these two are the same:

```toml
[package.metadata.binstall]
bin-dir = "{ name }-{ target }/bin"
# bin-dir = "{ name }-{ target }/bin/{ bin }{ binary-ext }"
```

Versions of cargo-binstall up to v1.27.0 do not support this and fail to find the binary, so use the full path if users may be on those.

### Installing a project that is not on crates.io

`binstall` takes the metadata above from a crate's `Cargo.toml`, so a project that
is not published on crates.io, or is not written in Rust at all, can still be
installed from its release downloads: write a `Cargo.toml` for it yourself and
pass it with `--manifest-path`.

For example, say the `v1.2.3` release of `https://github.com/example/mytool` has
the downloads `mytool-1.2.3-linux_amd64.tar.gz`, `mytool-1.2.3-darwin_arm64.tar.gz`
and `mytool-1.2.3-windows_amd64.zip`, each holding the `mytool` binary at the top:

```toml
[package]
name = "mytool"
version = "1.2.3"
repository = "https://github.com/example/mytool"

# Every binary to install needs a `[[bin]]` entry, since there are no sources
# to infer them from. `path` must be set, but the file does not need to exist.
[[bin]]
name = "mytool"
path = "main.rs"

[package.metadata.binstall]
pkg-url = "{ repo }/releases/download/v{ version }/{ name }-{ version }-{ os-name }_{ target-arch }{ archive-suffix }"
pkg-fmt = "tgz"
disabled-strategies = ["quick-install", "compile"]

[package.metadata.binstall.overrides.'cfg(target_os = "macos")']
pkg-url = "{ repo }/releases/download/v{ version }/{ name }-{ version }-darwin_{ target-arch }{ archive-suffix }"

[package.metadata.binstall.overrides.'cfg(target_os = "windows")']
pkg-fmt = "zip"
```

```console
$ cargo binstall --manifest-path path/to/Cargo.toml mytool
```

`{ target-arch }` becomes `amd64` and `arm64` here through the aliases listed for it above,
and `disabled-strategies` stops `binstall` from looking the name up on QuickInstall
or trying to build it with `cargo install`, which cannot work without sources.

The crate name on the command line must be the `name` in the file. The `version`
in the file is installed unless an exact version is given, like `mytool@1.2.4`
or `--version 1.2.4`, which is then used for `{ version }` instead, so the same
file installs any release named the same way. A version requirement such as
`^1.2` is not resolved, since there is no registry to list versions: the `version`
in the file is used, with a warning if it does not match.
