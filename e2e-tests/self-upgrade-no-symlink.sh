#!/bin/bash

set -euxo pipefail

# first bootstrap-install into the CARGO_HOME
cp "$1" "$CARGO_HOME/bin/"

# now we're running the CARGO_HOME/bin/cargo-binstall (via cargo):

# self update replacing no-symlinks with no-symlinks
cargo binstall --no-confirm --no-symlinks --force cargo-binstall@0.20.1

# self update replacing no-symlinks with symlinks
cp "$1" "$CARGO_HOME/bin/"

cargo binstall --no-confirm --force cargo-binstall@0.20.1

# self update replacing symlinks with symlinks
ln -snf "$1" "$CARGO_HOME/bin/"

cargo binstall --no-confirm --force cargo-binstall@0.20.1

# self update replacing symlinks with no-symlinks
ln -snf "$1" "$CARGO_HOME/bin/"

cargo binstall --no-confirm --force --no-symlinks cargo-binstall@0.20.1
