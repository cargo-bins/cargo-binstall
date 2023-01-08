#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

export CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export PATH="$CARGO_HOME/bin:$PATH"

# Test skip when installed
"./$1" binstall --no-confirm --force cargo-binstall@0.11.1
"./$1" binstall --log-level=info --no-confirm cargo-binstall@0.11.1 | grep -q 'cargo-binstall v0.11.1 is already installed'

"./$1" binstall --log-level=info --no-confirm cargo-binstall@0.10.0 | grep -q -v 'cargo-binstall v0.10.0 is already installed'

## Test When 0.11.0 is installed but can be upgraded.
"./$1" binstall --no-confirm cargo-binstall@0.12.0
"./$1" binstall --log-level=info --no-confirm cargo-binstall@0.12.0 | grep -q 'cargo-binstall v0.12.0 is already installed'
"./$1" binstall --log-level=info --no-confirm cargo-binstall@^0.12.0 | grep -q -v 'cargo-binstall v0.12.0 is already installed'
