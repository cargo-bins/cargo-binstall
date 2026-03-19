#!/bin/bash

set -euxo pipefail

# Install binaries using `--manifest-path`
# Also test default github template
"$1" binstall --force --manifest-path "manifests/private-github-repo-test-Cargo.toml" --no-confirm cargo-binstall --strategies crate-meta-data

# Test that the installed binaries can be run
cargo binstall --help >/dev/null

cargo_binstall_version="$(cargo binstall -V)"
echo "$cargo_binstall_version"

[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]
