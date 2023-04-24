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
