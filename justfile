# input variables
ci := env_var_or_default("CI", "")
for-release := env_var_or_default("JUST_FOR_RELEASE", "")
use-cross := env_var_or_default("JUST_USE_CROSS", "")
use-cargo-zigbuild  := env_var_or_default("JUST_USE_CARGO_ZIGBUILD", "")
extra-build-args := env_var_or_default("JUST_EXTRA_BUILD_ARGS", "")
extra-features := env_var_or_default("JUST_EXTRA_FEATURES", "")
default-features := env_var_or_default("JUST_DEFAULT_FEATURES", "")
override-features := env_var_or_default("JUST_OVERRIDE_FEATURES", "")
glibc-version := env_var_or_default("GLIBC_VERSION", "")
use-auditable := env_var_or_default("JUST_USE_AUDITABLE", "")
timings := env_var_or_default("JUST_TIMINGS", "")

export BINSTALL_LOG_LEVEL := if env_var_or_default("RUNNER_DEBUG", "0") == "1" { "debug" } else { "info" }
export BINSTALL_RATE_LIMIT := "30/1"

cargo := if use-cargo-zigbuild != "" { "cargo-zigbuild" } else if use-cross != "" { "cross" } else { "cargo" }
export CARGO := cargo

# target information
target-host := `rustc -vV | grep host: | cut -d ' ' -f 2`
target := env_var_or_default("CARGO_BUILD_TARGET", target-host)
target-os := if target =~ "-windows-" { "windows"
    } else if target =~ "darwin" { "macos"
    } else if target =~ "linux" { "linux"
    } else { "unknown" }
target-arch := if target =~ "x86_64" { "x64"
    } else if target =~ "i[56]86" { "x86"
    } else if target =~ "aarch64" { "arm64"
    } else if target =~ "armv7" { "arm32"
    } else { "unknown" }
target-libc := if target =~ "gnu" { "gnu"
    } else if target =~ "musl" { "musl"
    } else { "unknown" }

# build output location
output-ext := if target-os == "windows" { ".exe" } else { "" }
output-filename := "cargo-binstall" + output-ext
output-profile-folder := if for-release != "" { "release" } else { "debug" }
output-folder := "target" / target / output-profile-folder
output-path := output-folder / output-filename

# which tool to use for compiling
cargo-bin := if use-auditable != "" {
    "cargo-auditable auditable"
} else {
    cargo
}

# cargo compile options
cargo-profile := if for-release != "" { "release" } else { "dev" }


ci-or-no := if ci != "" { "ci" } else { "noci" }

# In release builds in CI, build the std library ourselves so it uses our
# compile profile, and optimise panic messages out with immediate abort.
cargo-buildstd := "" #if (cargo-profile / ci-or-no) == "release/ci" {
#" -Z build-std=std,panic_abort -Z build-std-features=panic_immediate_abort"
#} else { "" }

# In musl release builds in CI, statically link gcclibs.
rustc-gcclibs := if (cargo-profile / ci-or-no / target-libc) == "release/ci/musl" {
    if use-cargo-zigbuild != "" { "-C link-arg=-static-libgcc" } else { " -C link-arg=-lgcc -C link-arg=-static-libgcc" }
} else { "" }

# disable default features in CI for debug builds, for speed
cargo-no-default-features := if default-features == "false" { " --no-default-features"
    } else if default-features == "true" { ""
    } else if (cargo-profile / ci-or-no) == "dev/ci" { " --no-default-features"
    } else { "" }

support-pkg-config := if target == target-host {
    if target-os == "linux" { "true" } else { "" }
} else { "" }

#} else if target == "x86_64-unknown-linux-gnu" {
#    ",zlib-ng"
#} else if target == "x86_64-unknown-linux-musl" {
#    ",zlib-ng"
git-max-perf-feature := if target == "x86_64-apple-darwin" {
    ",zlib-ng"
} else if target == "aarch64-apple-darwin" {
    ",zlib-ng"
} else if target-os == "windows" {
    ",zlib-ng"
} else if target == "aarch64-unknown-linux-gnu" {
    ",zlib-ng"
} else if target == "aarch64-unknown-linux-musl" {
    ",zlib-ng"
} else {
    ""
}

cargo-features := trim_end_match(if override-features != "" { override-features
    } else if (cargo-profile / ci-or-no) == "dev/ci" { "git,rustls,fancy-with-backtrace,zstd-thin,log_max_level_debug" + git-max-perf-feature + (if support-pkg-config != "" { ",pkg-config" } else { "" }) + extra-features
    } else if (cargo-profile / ci-or-no) == "release/ci" { "git,static,rustls,trust-dns,fancy-no-backtrace,zstd-thin,log_release_max_level_debug,cross-lang-fat-lto"  + git-max-perf-feature + extra-features
    } else { extra-features
}, ",")

# it seems we can't split debuginfo for non-buildstd builds
# errors with: "Found a record with an unknown abbreviation code"
cargo-split-debuginfo := if cargo-buildstd != "" { " --config='profile.release.split-debuginfo=\"packed\"' --config=profile.release.debug=2" } else { "" }

# for ARM64 Windows, use a patched version of ring
# this should be unnecessary once ring 0.17 is released
win-arm64-ring16 := if target == "aarch64-pc-windows-msvc" { " --config='patch.crates-io.ring.git=\"https://github.com/awakecoding/ring\"' --config='patch.crates-io.ring.branch=\"0.16.20_alpha\"'" } else { "" }

# MIR optimisation level (defaults to 2, bring it up to 4 for release builds)
# **DISABLED because it's buggy**
rustc-miropt := "" # if for-release != "" { " -Z mir-opt-level=4" } else { "" }

# Use rust-lld that is bundled with rustup to speedup linking
# and support for icf=safe.
#
# -Zgcc-ld=lld uses the rust-lld that is bundled with rustup.
#
# TODO: There is ongoing effort to stabilise this and we will need to update
# this once it is merged.
# https://github.com/rust-lang/compiler-team/issues/510
#
# If cargo-zigbuild is used, then it will provide the lld linker.
# This option is disabled on windows since it not supported.
rust-lld := "" #if use-cargo-zigbuild != "" {
#""
#} else if target-os != "windows" {
#" -Z gcc-ld=lld"
#} else {
#""
#}

# ICF: link-time identical code folding
#
# On windows it works out of the box and on linux it uses
# rust-lld.
rustc-icf := if for-release != "" {
    if target-os == "windows" {
        " -C link-arg=-Wl,--icf=safe"
     } else if target-os == "linux" {
        " -C link-arg=-Wl,--icf=safe"
     } else {
        ""
    }
} else {
    ""
}

# Only enable linker-plugin-lto for release
# Also disable this on windows since it uses msvc.
#
# Temporarily disable this on linux due to mismatch llvm version
# } else if target-os == "linux" {
#     "-C linker-plugin-lto "
linker-plugin-lto := if for-release == "" {
    ""
} else {
    ""
}

target-glibc-ver-postfix := if glibc-version != "" {
    if use-cargo-zigbuild != "" {
        "." + glibc-version
    } else {
        ""
    }
} else {
    ""
}

cargo-check-args := (" --target ") + (target) + (target-glibc-ver-postfix) + (cargo-buildstd) + (if extra-build-args != "" { " " + extra-build-args } else { "" }) + (cargo-split-debuginfo) + (win-arm64-ring16)
cargo-build-args := (if for-release != "" { " --release" } else { "" }) + (cargo-check-args) + (cargo-no-default-features) + (if cargo-features != "" { " --features " + cargo-features } else { "" }) + (if timings != "" { " --timings" } else { "" })
export RUSTFLAGS := (linker-plugin-lto) + (rustc-gcclibs) + (rustc-miropt) + (rust-lld) + (rustc-icf)


# libblocksruntime-dev provides compiler-rt
ci-apt-deps := if target == "x86_64-unknown-linux-gnu" { "liblzma-dev libzip-dev libzstd-dev"
    } else { "" }

[linux]
ci-install-deps:
    if [ -n "{{ci-apt-deps}}" ]; then sudo apt update && sudo apt install -y --no-install-recommends {{ci-apt-deps}}; fi
    if [ -n "{{use-cargo-zigbuild}}" ]; then pip3 install cargo-zigbuild; fi

[macos]
[windows]
ci-install-deps:

toolchain components="":
    rustup toolchain install stable {{ if components != "" { "--component " + components } else { "" } }} --no-self-update --profile minimal
    {{ if ci != "" { "rustup default stable" } else { "rustup override set stable" } }}
    {{ if target != "" { "rustup target add " + target } else { "" } }}

print-env:
    @echo "env RUSTFLAGS='$RUSTFLAGS', CARGO='$CARGO'"

print-rustflags:
    @echo "$RUSTFLAGS"

build: print-env
    {{cargo-bin}} build {{cargo-build-args}}

check: print-env
    {{cargo-bin}} check {{cargo-build-args}} --profile check-only
    cargo-hack hack check --feature-powerset -p leon {{cargo-check-args}} --profile check-only
    {{cargo-bin}} check -p binstalk-downloader --no-default-features --profile check-only
    {{cargo-bin}} check -p cargo-binstall --no-default-features --features rustls {{cargo-check-args}} --profile check-only
    cargo-hack hack check -p binstalk-downloader \
        --feature-powerset \
        --include-features default,json,gh-api-client \
        --profile check-only \
        {{cargo-check-args}}

get-output file outdir=".":
    test -d "{{outdir}}" || mkdir -p {{outdir}}
    cp -r {{ output-folder / file }} {{outdir}}/{{ file_name(file) }}
    -ls -l {{outdir}}/{{ file_name(file) }}

get-binary outdir=".": (get-output output-filename outdir)
    -chmod +x {{ outdir / output-filename }}

e2e-test file *arguments: (get-binary "e2e-tests")
    cd e2e-tests && env -u RUSTFLAGS bash {{file}}.sh {{output-filename}} {{arguments}}

e2e-test-live: (e2e-test "live")
e2e-test-subcrate: (e2e-test "subcrate")
e2e-test-manifest-path: (e2e-test "manifest-path")
e2e-test-other-repos: (e2e-test "other-repos")
e2e-test-strategies: (e2e-test "strategies")
e2e-test-version-syntax: (e2e-test "version-syntax")
e2e-test-upgrade: (e2e-test "upgrade")
e2e-test-self-upgrade-no-symlink: (e2e-test "self-upgrade-no-symlink")
e2e-test-uninstall: (e2e-test "uninstall")
e2e-test-no-track: (e2e-test "no-track")
e2e-test-git: (e2e-test "git")
e2e-test-registries: (e2e-test "registries")

# WinTLS (Windows in CI) does not have TLS 1.3 support
[windows]
e2e-test-tls: (e2e-test "tls" "1.2")
[linux]
[macos]
e2e-test-tls: (e2e-test "tls" "1.2") (e2e-test "tls" "1.3")

e2e-tests: e2e-test-live e2e-test-manifest-path e2e-test-git e2e-test-other-repos e2e-test-strategies e2e-test-version-syntax e2e-test-upgrade e2e-test-tls e2e-test-self-upgrade-no-symlink e2e-test-uninstall e2e-test-subcrate e2e-test-no-track e2e-test-registries

unit-tests: print-env
    {{cargo-bin}} test {{cargo-build-args}}

test: unit-tests build e2e-tests

clippy: print-env
    {{cargo-bin}} clippy --no-deps -- -D clippy::all

doc: print-env
    cargo doc --no-deps --workspace

fmt: print-env
    cargo fmt --all -- --check

fmt-check: fmt

lint: clippy fmt-check doc

# Rm dev-dependencies for `cargo-check` and clippy to speedup compilation.
# This is a workaround for the cargo nightly option `-Z avoid-dev-deps`
avoid-dev-deps:
    for crate in ./crates/*; do \
        sed 's/\[dev-dependencies\]/[workaround-avoid-dev-deps]/g' "$crate/Cargo.toml" >"$crate/Cargo.toml.tmp"; \
        mv "$crate/Cargo.toml.tmp" "$crate/Cargo.toml" \
    ; done

package-dir:
    rm -rf packages/prep
    mkdir -p packages/prep
    cp crates/bin/LICENSE packages/prep
    cp README.md packages/prep

[macos]
package-prepare: build package-dir
    just get-binary packages/prep
    -just get-output cargo-binstall.dSYM packages/prep

    just get-output detect-wasi{{output-ext}} packages/prep
    -just get-output detect-wasi.dSYM packages/prep

# when https://github.com/rust-lang/cargo/pull/11384 lands, we can use
# -just get-output cargo_binstall.dwp packages/prep
# underscored dwp name needs to remain for debuggers to find the file properly
[linux]
package-prepare: build package-dir
    just get-binary packages/prep
    -cp {{output-folder}}/deps/cargo_binstall-*.dwp packages/prep/cargo_binstall.dwp

    just get-output detect-wasi packages/prep
    -cp {{output-folder}}/deps/detect_wasi-*.dwp packages/prep/detect_wasi.dwp

# underscored pdb name needs to remain for debuggers to find the file properly
# read from deps because sometimes cargo doesn't copy the pdb to the output folder
[windows]
package-prepare: build package-dir
    just get-binary packages/prep
    -just get-output deps/cargo_binstall.pdb packages/prep

    just get-output detect-wasi.exe packages/prep
    -just get-output deps/detect_wasi.pdb packages/prep

# we don't get dSYM bundles for universal binaries; unsure if it's even a thing
[macos]
lipo-prepare: package-dir
    just target=aarch64-apple-darwin build get-binary packages/prep/arm64
    just target=x86_64-apple-darwin build get-binary packages/prep/x64

    just target=aarch64-apple-darwin get-binary packages/prep/arm64
    just target=x86_64-apple-darwin get-binary packages/prep/x64
    lipo -create -output packages/prep/{{output-filename}} packages/prep/{arm64,x64}/{{output-filename}}

    just target=aarch64-apple-darwin get-output detect-wasi{{output-ext}} packages/prep/arm64
    just target=x86_64-apple-darwin get-output detect-wasi{{output-ext}} packages/prep/x64
    lipo -create -output packages/prep/detect-wasi{{output-ext}} packages/prep/{arm64,x64}/detect-wasi{{output-ext}}

    rm -rf packages/prep/{arm64,x64}


[linux]
package: package-prepare
    cd packages/prep && tar cv {{output-filename}} | gzip -9 > "../cargo-binstall-{{target}}.tgz"
    cd packages/prep && tar cv * | gzip -9 > "../cargo-binstall-{{target}}.full.tgz"

[macos]
package: package-prepare
    cd packages/prep && zip -r -9 "../cargo-binstall-{{target}}.zip" {{output-filename}}
    cd packages/prep && zip -r -9 "../cargo-binstall-{{target}}.full.zip" *

[windows]
package: package-prepare
    cd packages/prep && 7z a -mx9 "../cargo-binstall-{{target}}.zip" {{output-filename}}
    cd packages/prep && 7z a -mx9 "../cargo-binstall-{{target}}.full.zip" *

[macos]
package-lipo: lipo-prepare
    cd packages/prep && zip -r -9 "../cargo-binstall-universal-apple-darwin.zip" {{output-filename}}
    cd packages/prep && zip -r -9 "../cargo-binstall-universal-apple-darwin.full.zip" *

# assuming x64 and arm64 packages are already built, extract and lipo them
[macos]
repackage-lipo: package-dir
    mkdir -p packages/prep/{arm64,x64}
    cd packages/prep/x64 && unzip -o "../../cargo-binstall-x86_64-apple-darwin.full.zip"
    cd packages/prep/arm64 && unzip -o "../../cargo-binstall-aarch64-apple-darwin.full.zip"

    lipo -create -output packages/prep/{{output-filename}} packages/prep/{arm64,x64}/{{output-filename}}
    lipo -create -output packages/prep/detect-wasi packages/prep/{arm64,x64}/detect-wasi

    rm -rf packages/prep/{arm64,x64}
    cd packages/prep && zip -9 "../cargo-binstall-universal-apple-darwin.zip" {{output-filename}}
    cd packages/prep && zip -9 "../cargo-binstall-universal-apple-darwin.full.zip" *
