#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

"./$1" binstall -y cargo-binstall@0.20.1
cargo-binstall --help >/dev/null

set +e

"./$1" binstall -y --no-track cargo-binstall@0.20.1
exit_code="$?"

set -e

if [ "$exit_code" != 88 ]; then
    echo "Expected exit code 88 BinFile Error, but actual exit code $exit_code"
    exit 1
fi


"./$1" binstall -y --no-track --force cargo-binstall@0.20.1
cargo-binstall --help >/dev/null
