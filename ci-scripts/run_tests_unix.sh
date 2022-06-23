#!/bin/bash

set -euxo pipefail

bins="cargo-deb cargo-llvm-cov cargo-binstall"
test_bins="cargo-deb cargo-llvm-cov"

# Install binaries using cargo-binstall
for bin in $bins; do
    "./$1" binstall --log-level debug --no-confirm "$bin"
done

# Test that the installed binaries can be run
for bin in $test_bins; do
    "$HOME/.cargo/bin/$bin" --version
done
cargo binstall --help >/dev/null

# Install binaries using `--manifest-path`
"./$1" binstall --log-level debug --manifest-path . --no-confirm cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null

# Install binaries using https-only-mode and specify min tls ver
"./$1" binstall \
    --log-level debug \
    --https-only-mode \
    --min-tls-version tls1-3 \
    --no-confirm \
    cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null
