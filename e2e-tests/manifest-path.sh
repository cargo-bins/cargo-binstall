#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

# Install binaries using `--manifest-path`
# Also test default github template
"./$1" binstall --force --manifest-path "manifests/github-test-Cargo.toml" --no-confirm cargo-binstall

# Test that the installed binaries can be run
cargo binstall --help >/dev/null

cargo_binstall_version="$(cargo binstall -V)"
echo "$cargo_binstall_version"

[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]

cat "$CARGO_HOME/.crates.toml"
grep -F "cargo-binstall 0.12.0 (path+file://manifests/github-test-Cargo.toml)" <"$CARGO_HOME/.crates.toml"

# Test that `--manifest-path` can handle relative path without parent well
exe="$(realpath "./$1")"
cd manifests
"$exe" binstall --force --manifest-path github-test-Cargo.toml --no-confirm cargo-binstall

cargo_binstall_version="$(cargo binstall -V)"
echo "$cargo_binstall_version"

[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]
