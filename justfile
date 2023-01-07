# input variables
ci := env_var_or_default("CI", "")
for-release := env_var_or_default("JUST_FOR_RELEASE", "")
use-cross := env_var_or_default("JUST_USE_CROSS", "")
extra-build-args := env_var_or_default("JUST_EXTRA_BUILD_ARGS", "")
extra-features := env_var_or_default("JUST_EXTRA_FEATURES", "")
default-features := env_var_or_default("JUST_DEFAULT_FEATURES", "")
override-features := env_var_or_default("JUST_OVERRIDE_FEATURES", "")

export BINSTALL_LOG_LEVEL := if env_var_or_default("RUNNER_DEBUG", "0") == "1" { "debug" } else { "info" }

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
output-filename := if target-os == "windows" { "cargo-binstall.exe" } else { "cargo-binstall" }
output-profile-folder := if for-release != "" { "release" } else { "debug" }
output-folder := if target != target-host { "target" / target / output-profile-folder
    } else if env_var_or_default("CARGO_BUILD_TARGET", "") != "" { "target" / target / output-profile-folder
    } else if cargo-buildstd != "" { "target" / target / output-profile-folder
    } else { "target" / output-profile-folder }
output-path := output-folder / output-filename

# which tool to use for compiling
cargo-bin := if use-cross != "" { "cross" } else { "cargo +nightly" }

# cargo compile options
cargo-profile := if for-release != "" { "release" } else { "dev" }


ci-or-no := if ci != "" { "ci" } else { "noci" }

# In release builds in CI, build the std library ourselves so it uses our
# compile profile, and optimise panic messages out with immediate abort.
#
# explicitly disabled on aarch64-unknown-linux-gnu due to a failing build
cargo-buildstd := if (cargo-profile / ci-or-no) == "release/ci" {
    if target == "aarch64-unknown-linux-gnu" { ""
    } else { " -Z build-std=std,panic_abort -Z build-std-features=panic_immediate_abort" }
} else { "" }

# In musl release builds in CI, statically link gcclibs.
cargo-gcclibs := if (cargo-profile / ci-or-no / target-libc) == "release/ci/musl" {
    " -C link-arg=-lgcc -C link-arg=-static-libgcc"
} else { "" }

# disable default features in CI for debug builds, for speed
cargo-no-default-features := if default-features == "false" { " --no-default-features"
    } else if default-features == "true" { ""
    } else if (cargo-profile / ci-or-no) == "dev/ci" { " --no-default-features"
    } else { "" }

cargo-features := trim_end_match(if override-features != "" { override-features
    } else if (cargo-profile / ci-or-no) == "dev/ci" { "rustls,fancy-with-backtrace," + extra-features
    } else if (cargo-profile / ci-or-no / target-libc) == "release/ci/musl" { "rustls,fancy-with-backtrace,zstd-thin," + extra-features
    } else if (cargo-profile / ci-or-no) == "release/ci" { "rustls,fancy-with-backtrace," + extra-features
    } else { extra-features
}, ",")

cargo-build-args := (if for-release != "" { " --release" } else { "" }) + (if ci != "" { " --locked" } else { "" }) + (if target != target-host { " --target " + target } else if cargo-buildstd != "" { " --target " + target } else { "" }) + (cargo-buildstd) + (if extra-build-args != "" { " " + extra-build-args } else { "" }) + (cargo-no-default-features) + (if cargo-features != "" { " --features " + cargo-features } else { "" })
export RUSTFLAGS := (cargo-gcclibs)


ci-apt-deps := if target == "x86_64-unknown-linux-gnu" { "liblzma-dev libzip-dev libzstd-dev"
    } else if target == "x86_64-unknown-linux-musl" { "musl-tools"
    } else { "" }

[linux]
ci-install-deps:
    {{ if ci-apt-deps == "" { "exit" } else { "" } }}
    sudo apt update && sudo apt install -y --no-install-recommends {{ci-apt-deps}}

[macos]
[windows]
ci-install-deps:

toolchain components="":
    rustup toolchain install nightly {{ if components != "" { "--component " + components } else { "" } }} --no-self-update --profile minimal
    {{ if ci != "" { "rustup default nightly" } else { "rustup override set nightly" } }}
    {{ if target != "" { "rustup target add " + target } else { "" } }}


build:
    {{cargo-bin}} build {{cargo-build-args}}

check:
    {{cargo-bin}} check {{cargo-build-args}}

get-binary output=output-filename:
    {{ if output =~ "/" { "mkdir -p " + parent_directory(output) } else { "" } }}
    cp {{output-path}} {{output}}
    -chmod +x {{output}}
    -ls -l {{output}}

e2e-test file *arguments: (get-binary ("e2e-tests" / output-filename))
    cd e2e-tests && bash {{file}}.sh {{output-filename}} {{arguments}}

e2e-test-live: (e2e-test "live")
e2e-test-manifest-path: (e2e-test "manifest-path")
e2e-test-other-repos: (e2e-test "other-repos")
e2e-test-strategies: (e2e-test "strategies")
e2e-test-version-syntax: (e2e-test "version-syntax")
e2e-test-upgrade: (e2e-test "upgrade")

# WinTLS (Windows in CI) does not have TLS 1.3 support
[windows]
e2e-test-tls: (e2e-test "tls" "1.2")
[linux]
[macos]
e2e-test-tls: (e2e-test "tls" "1.2") (e2e-test "tls" "1.3")

e2e-tests: e2e-test-live e2e-test-manifest-path e2e-test-other-repos e2e-test-strategies e2e-test-version-syntax e2e-test-upgrade e2e-test-tls

unit-tests:
    {{cargo-bin}} test {{cargo-build-args}}

test: unit-tests build e2e-tests

clippy:
    {{cargo-bin}} clippy --no-deps -- -D clippy::all

fmt:
    cargo fmt --all -- --check

fmt-check:
    cargo fmt --all -- --check

lint: clippy fmt-check

package-dir:
    mkdir -p packages

[linux]
package: build get-binary package-dir
    tar cv {{output-filename}} | gzip -9 > "packages/cargo-binstall-{{target}}.tgz"

[macos]
[windows]
package: build get-binary package-dir
    zip -9 "packages/cargo-binstall-{{target}}.zip" {{output-filename}}

[macos]
package-lipo: lipo package-dir
    zip -9 "packages/cargo-binstall-universal-apple-darwin.zip" {{output-filename}}

[macos]
lipo:
    just target=aarch64-apple-darwin build get-binary=arm64/{{output-filename}}
    just target=x86_64-apple-darwin build get-binary=x64/{{output-filename}}
    lipo -create -output {{output-filename}} arm64/{{output-filename}} x64/{{output-filename}}
