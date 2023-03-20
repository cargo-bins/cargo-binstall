#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

crates="b3sum@1.3.3 cargo-release@0.24.5 cargo-binstall@0.20.1 cargo-watch@8.4.0 miniserve@0.23.0 sccache@0.3.3"

export CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
othertmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-test')
export PATH="$CARGO_HOME/bin:$othertmpdir/bin:$PATH"

mkdir -p "$othertmpdir/bin"
# Copy it to bin to test use of env var `CARGO`
cp "./$1" "$othertmpdir/bin/"

# Install binaries using cargo-binstall
# shellcheck disable=SC2086
cargo binstall --no-confirm $crates

rm -r "$othertmpdir"

# Test that the installed binaries can be run
b3sum --version
cargo-release release --version
cargo-binstall --help >/dev/null
cargo binstall --help >/dev/null
cargo watch -V
miniserve -V
