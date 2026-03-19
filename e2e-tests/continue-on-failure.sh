#!/bin/bash

set -uxo pipefail

## Test --continue-on-failur
"$1" --no-confirm --continue-on-failure cargo-watch@8.4.0 non-existent-clippy
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
"$1" --no-confirm --continue-on-failure non-existent-clippy
exit_code="$?"

set -e

if [ "$exit_code" != 76 ]; then
    echo "Expected exit code 76, but actual exit code $exit_code"
    exit 1
fi

# Test if both crates are invalid
set +e
"$1" --no-confirm --continue-on-failure non-existent-clippy non-existent-clippy2
exit_code="$?"

set -e

if [ "$exit_code" != 76 ]; then
    echo "Expected exit code 76, but actual exit code $exit_code"
    exit 1
fi
