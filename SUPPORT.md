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
- `bin-dir` specifies the binary path within the package, templated (with an `.exe` suffix on windows)
- `pkg-fmt` overrides the package format for download/extraction (defaults to: `tgz`), check [the documentation](https://docs.rs/binstalk-types/latest/binstalk_types/cargo_toml_binstall/enum.PkgFmt.html) for all supported formats.
- `disabled-strategies` to disable specific strategies (e.g. `crate-meta-data` for trying to find pre-built on your repository,
  `quick-install` for pre-built from third-party cargo-bins/cargo-quickinstall, `compile` for falling back to `cargo-install`)
  for your crate (defaults to empty array).
  If `--strategies` is passed on the command line, then the `disabled-strategies` in `package.metadata` will be ignored.
  Otherwise, the `disabled-strategies` in `package.metadata` and `--disable-strategies` will be merged.


`pkg-url` and `bin-dir` are templated to support different names for different versions / architectures / etc.
Template variables use the format `{ VAR }` where `VAR` is the name of the variable,
`\{` for literal `{`, `\}` for literal `}` and `\\` for literal `\`,
with the following variables available:
- `name` is the name of the crate/package
- `version` is the crate version (per `--version` and the crate manifest)
- `repo` is the repository linked in `Cargo.toml`
- `bin` is the name of a specific binary, inferred from the crate configuration
- `target` is the rust target name (defaults to your architecture, but can be overridden using the `--target` command line option if required)
- `archive-suffix` is the filename extension of the package archive format that includes the prefix `.`, e.g. `.tgz` for tgz or `.exe`/`""` for bin.
- `archive-format` is the soft-deprecated filename extension of the package archive format that does not include the prefix `.`, e.g. `tgz` for tgz or `exe`/`""` for bin.
- `binary-ext` is the string `.exe` if the `target` is for Windows, or the empty string otherwise
- `format` is a soft-deprecated alias for `archive-format` in `pkg-url`, and alias for `binary-ext` in `bin-dir`; in the future, this may warn at install time.
- `target-family`: Operating system of the target from [`target_lexicon::OperatingSystem`]
- `target-arch`: Architecture of the target, `universal` on `{universal, universal2}-apple-darwin`,
  otherwise from [`target_lexicon::Architecture`]
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

### Defaults

By default, `binstall` will try all supported package formats and would do the same for `bin-dir`.

It will first extract the archives, then iterate over the following list, finding the first dir
that exists:

 - `{ name }-{ target }-v{ version }`
 - `{ name }-{ target }-{ version }`
 - `{ name }-{ version }-{ target }`
 - `{ name }-v{ version }-{ target }`
 - `{ name }-{ target }`
 - `{ name }-{ version }`
 - `{ name }-v{ version }`
 - `{ name }`

Then it will concat the dir with `"{ bin }{ binary-ext }"` and use that as the final `bin-dir`.

`name` here is name of the crate, `bin` is the cargo binary name and `binary-ext` is `.exe`
on windows and empty on other platforms).

The default value for `pkg-url` will depend on the repository of the package.

It is set up to work with GitHub releases, GitLab releases, bitbucket downloads,
source forge downloads and Codeberg releases.

If your package already uses any of these URLs, you shouldn't need to set anything.

The URLs are derived from a set of filenames and a set of paths, which are
"multiplied together": every filename appended to every path. The filenames
are:

- `{ name }-{ target }-{ version }{ archive-suffix }`
- `{ name }-{ target }-v{ version }{ archive-suffix }`
- `{ name }-{ version }-{ target }{ archive-suffix }`
- `{ name }-v{ version }-{ target }{ archive-suffix }`
- `{ name }_{ target }_{ version }{ archive-suffix }`
- `{ name }_{ target }_v{ version }{ archive-suffix }`
- `{ name }_{ version }_{ target }{ archive-suffix }`
- `{ name }_v{ version }_{ target }{ archive-suffix }`
- `{ name }-{ target }{ archive-suffix }` ("versionless")
- `{ name }_{ target }{ archive-suffix }` ("versionless")

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

#### for Codeberg

- `{ repo }/releases/download/{ version }/`
- `{ repo }/releases/download/v{ version }/`

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

Which provides a binary path of: `sx128x-util-x86_64-unknown-linux-gnu[.exe]`. It is worth noting that binary names are inferred from the crate, so as long as cargo builds them this _should_ just work.
