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
min_tls=1.3
[[ "${2:-}" == "Windows" ]] && min_tls=1.2 # WinTLS on GHA doesn't support 1.3 yet

"./$1" binstall \
    --log-level debug \
    --secure \
    --min-tls-version $min_tls \
    --no-confirm \
    cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null
