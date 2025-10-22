#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

# first bootstrap-install into the CARGO_HOME
mkdir -p "$CARGO_HOME/bin"
cp "./$1" "$CARGO_HOME/bin"

# now we're running the CARGO_HOME/bin/cargo-binstall (via cargo):

# self update replacing no-symlinks with no-symlinks
cargo binstall --no-confirm --no-symlinks --force cargo-binstall@0.20.1

# self update replacing no-symlinks with symlinks
cp "./$1" "$CARGO_HOME/bin"

cargo binstall --no-confirm --force cargo-binstall@0.20.1

# self update replacing symlinks with symlinks
ln -snf "$(pwd)/cargo-binstall" "$CARGO_HOME/bin/cargo-binstall"

cargo binstall --no-confirm --force cargo-binstall@0.20.1

# self update replacing symlinks with no-symlinks
ln -snf "$(pwd)/cargo-binstall" "$CARGO_HOME/bin/cargo-binstall"

cargo binstall --no-confirm --force --no-symlinks cargo-binstall@0.20.1
