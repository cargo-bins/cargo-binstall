#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

export CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export PATH="$CARGO_HOME/bin:$PATH"

# Test --version
"./$1" binstall --force --no-confirm --version 0.11.1 cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null

# Test "$crate_name@$version"
"./$1" binstall --force --no-confirm cargo-binstall@0.11.1
# Test that the installed binaries can be run
cargo binstall --help >/dev/null
