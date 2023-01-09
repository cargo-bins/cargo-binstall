#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

export CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export PATH="$CARGO_HOME/bin:$PATH"

"./$1" binstall \
    --force \
    --min-tls-version "${2:-1.3}" \
    --no-confirm \
    cargo-binstall
# Test that the installed binaries can be run
cargo binstall --help >/dev/null
