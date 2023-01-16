#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

export CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export PATH="$CARGO_HOME/bin:$PATH"

# first boostrap-install into the CARGO_HOME
mkdir -p "$CARGO_HOME/bin"
cp "./$1" "$CARGO_HOME/bin"

# now we're running the CARGO_HOME/bin/cargo-binstall (via cargo):

# self update
cargo binstall --no-confirm --no-symlinks --force cargo-binstall

## self update replacing no-symlinks with symlinks
#cargo binstall --no-confirm --force cargo-binstall
#
## self update with symlinks
#cargo binstall --no-confirm --force cargo-binstall
