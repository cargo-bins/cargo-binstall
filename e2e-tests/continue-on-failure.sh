#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
othertmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-test')
export PATH="$CARGO_HOME/bin:$othertmpdir/bin:$PATH"

mkdir -p "$othertmpdir/bin"
# Copy it to bin to test use of env var `CARGO`
cp "./$1" "$othertmpdir/bin/"


## Test --continue-on-failure
set +e
cargo binstall --no-confirm --continue-on-failure cargo-watch@8.4.0 non-existent-clippy
exit_code="$?"

set -e

if [ "$exit_code" != 76 ]; then
    echo "Expected exit code 76, but actual exit code $exit_code"
    exit 1
fi


cargo_watch_version="$(cargo watch -V)"
echo "$cargo_watch_version"

[ "$cargo_watch_version" = "cargo-watch 8.4.0" ]


## Test that it is no-op when only one crate is passed
set +e
cargo binstall --no-confirm --continue-on-failure non-existent-clippy
exit_code="$?"

set -e

if [ "$exit_code" != 76 ]; then
    echo "Expected exit code 76, but actual exit code $exit_code"
    exit 1
fi

# Test if both crates are invalid
set +e
cargo binstall --no-confirm --continue-on-failure non-existent-clippy non-existent-clippy2
exit_code="$?"

set -e

if [ "$exit_code" != 76 ]; then
    echo "Expected exit code 76, but actual exit code $exit_code"
    exit 1
fi
