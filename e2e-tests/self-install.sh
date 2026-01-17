#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

TMPDIR=$(mktemp -d 2>/dev/null || mktemp -d -t tmp)
cp "./$1" "$TMPDIR/cargo-binstall"
"$TMPDIR/cargo-binstall" --self-install

cargo binstall -vV
cargo install --list
