#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

export CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export PATH="$CARGO_HOME/bin:$PATH"

"./$1" binstall --no-confirm --no-symlinks --force cargo-binstall@0.11.1

"./$1" binstall --no-confirm --force cargo-binstall@0.12.0
