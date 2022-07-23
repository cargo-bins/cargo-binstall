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
"./$1" binstall --log-level debug --manifest-path . --no-confirm cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null

# Install binaries using secure mode
"./$1" binstall \
    --log-level debug \
    --secure \
    --min-tls-version 1.3 \
    --no-confirm \
    cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null
