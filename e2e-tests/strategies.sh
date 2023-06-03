#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

## Test --disable-strategies
set +e

"./$1" binstall --no-confirm --disable-strategies quick-install,compile cargo-update@11.1.2
exit_code="$?"

set -e

if [ "$exit_code" != 94 ]; then
    echo "Expected exit code 94, but actual exit code $exit_code"
    exit 1
fi

## Test --strategies
set +e

"./$1" binstall --no-confirm --strategies crate-meta-data cargo-update@11.1.2
exit_code="$?"

set -e

if [ "$exit_code" != 94 ]; then
    echo "Expected exit code 94, but actual exit code $exit_code"
    exit 1
fi

## Test compile-only strategy
"./$1" binstall --no-confirm --strategies compile cargo-quickinstall@0.2.8
