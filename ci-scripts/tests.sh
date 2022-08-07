#!/bin/bash

set -euxo pipefail

bins="cargo-deb cargo-llvm-cov cargo-binstall"
test_bins="cargo-deb cargo-llvm-cov"

unset CARGO_INSTALL_ROOT
unset CARGO_HOME

# Install binaries using cargo-binstall
# shellcheck disable=SC2086
"./$1" binstall --log-level debug --no-confirm $bins

# Test that the installed binaries can be run
for bin in $test_bins; do
    "$HOME/.cargo/bin/$bin" --version
done
cargo binstall --help >/dev/null

# Install binaries using `--manifest-path`
"./$1" binstall --force --log-level debug --manifest-path . --no-confirm cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null

# Install binaries using secure mode
min_tls=1.3
[[ "${2:-}" == "Windows" ]] && min_tls=1.2 # WinTLS on GHA doesn't support 1.3 yet

"./$1" binstall \
    --force \
    --log-level debug \
    --secure \
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
"./$1" binstall --no-confirm cargo-binstall | grep -q 'package cargo-binstall is already installed'
"./$1" binstall --no-confirm cargo-binstall@0.11.1 | grep -q 'package cargo-binstall is already installed'

"./$1" binstall --no-confirm cargo-binstall@0.10.0 | grep -q -v 'package cargo-binstall is already installed'
