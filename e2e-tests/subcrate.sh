#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
othertmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-test')
export PATH="$CARGO_HOME/bin:$othertmpdir/bin:$PATH"

mkdir -p "$othertmpdir/bin"
# Copy it to bin to test use of env var `CARGO`
cp "./$1" "$othertmpdir/bin/"

cargo binstall --no-confirm cargo-audit@0.17.5 --strategies crate-meta-data

cargo audit --version
