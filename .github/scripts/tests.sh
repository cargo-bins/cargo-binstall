#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

crates="b3sum cargo-release cargo-binstall cargo-watch miniserve sccache"

if [ "$2" = "Windows" ]; then
    # Install binaries using cargo-binstall
    # shellcheck disable=SC2086
    "./$1" --log-level debug --no-confirm $crates
else
    export CARGO_HOME=/tmp/cargo-home-for-test
    export PATH="$CARGO_HOME/bin:$PATH"
    
    mkdir -p "$CARGO_HOME/bin"
    # Copy it to bin to test use of env var `CARGO`
    cp "./$1" "$CARGO_HOME/bin/cargo-binstall"
    
    # Install binaries using cargo-binstall
    # shellcheck disable=SC2086
    cargo binstall --log-level debug --no-confirm $crates
fi

# Test that the installed binaries can be run
b3sum --version
cargo-release release --version
cargo-binstall --help >/dev/null
cargo binstall --help >/dev/null
cargo watch -V
miniserve -V

test_resources=".github/scripts/cargo-tomls"

# Install binaries using `--manifest-path`
# Also test default github template
"./$1" binstall --force --log-level debug --manifest-path "$test_resources/github-test-Cargo.toml" --no-confirm cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null

# FIXME: test this some other way that is not dependent on the version being published!
# "./$1" binstall --force --log-level debug --manifest-path crates/bin --no-confirm cargo-binstall

min_tls=1.3
[[ "${2:-}" == "Windows" ]] && min_tls=1.2 # WinTLS on GHA doesn't support 1.3 yet

"./$1" binstall \
    --force \
    --log-level debug \
    --min-tls-version $min_tls \
    --no-confirm \
    cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null

# Test --version
"./$1" binstall --force --log-level debug --no-confirm --version 0.11.1 cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null

# Test "$crate_name@$version"
"./$1" binstall --force --log-level debug --no-confirm cargo-binstall@0.11.1
# Test that the installed binaries can be run
cargo binstall --help >/dev/null

# Test skip when installed
"./$1" binstall --no-confirm --force cargo-binstall@0.11.1
"./$1" binstall --no-confirm cargo-binstall@0.11.1 | grep -q 'cargo-binstall v0.11.1 is already installed'

"./$1" binstall --no-confirm cargo-binstall@0.10.0 | grep -q -v 'cargo-binstall v0.10.0 is already installed'

## Test When 0.11.0 is installed but can be upgraded.
"./$1" binstall --no-confirm cargo-binstall@0.12.0
"./$1" binstall --no-confirm cargo-binstall@0.12.0 | grep -q 'cargo-binstall v0.12.0 is already installed'
"./$1" binstall --no-confirm cargo-binstall@^0.12.0 | grep -q -v 'cargo-binstall v0.12.0 is already installed'

# to force failure if falling back to source
# FIXME: remove/replace once #136 lands
export PATH="$test_resources/fake-cargo:$PATH"

# Test default GitLab pkg-url templates
"./$1" binstall \
    --force \
    --manifest-path "$test_resources/gitlab-test-Cargo.toml" \
    --log-level debug \
    --no-confirm \
    cargo-binstall

# Test default BitBucket pkg-url templates
"./$1" binstall \
    --force \
    --manifest-path "$test_resources/bitbucket-test-Cargo.toml" \
    --log-level debug \
    --no-confirm \
    cargo-binstall

# Test default Github pkg-url templates,
# with bin-dir provided
"./$1" binstall \
    --force \
    --manifest-path "$test_resources/github-test-Cargo2.toml" \
    --log-level debug \
    --no-confirm \
    cargo-binstall
