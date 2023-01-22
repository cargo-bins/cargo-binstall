#!/bin/bash

set -uxo pipefail

unset CARGO_INSTALL_ROOT

export CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export PATH="$CARGO_HOME/bin:$PATH"

## Test --disable-strategies
"./$1" binstall --no-confirm --disable-strategies quick-install,compile cargo-update
exit_code="$?"

if [ "$exit_code" != 94 ]; then
    echo "Expected exit code 94, but actual exit code $exit_code"
    exit 1
fi

## Test --strategies
"./$1" binstall --no-confirm --strategies crate-meta-data cargo-update
exit_code="$?"

if [ "$exit_code" != 94 ]; then
    echo "Expected exit code 94, but actual exit code $exit_code"
    exit 1
fi

## Test compile-only strategy
"./$1" binstall --no-confirm --strategies compile cargo-quickinstall
