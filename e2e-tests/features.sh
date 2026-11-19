#!/bin/bash

set -euxo pipefail

# Test for the features flag
#
# cargo-lock's `cargo-lock` binary is gated behind the `cli` feature, so
# compiling it from source fails unless `--features=cli` is passed through
# to `cargo install`. It has few dependencies, so it builds quickly.
"$1" binstall --no-confirm \
    cargo-lock --version 11.1.0 --locked --force \
    --strategies compile \
    --features cli

# Verify that the binary was installed and is executable
if ! command -v cargo-lock >/dev/null 2>&1; then
  echo "cargo-lock was not installed"
  exit 1
fi

# Run the binary to check it works
cargo-lock --help

# Verify that --features cannot be combined with multiple packages
set +e

"$1" binstall --no-confirm cargo-binstall cargo-lock --strategies compile --features cli
exit_code="$?"

set -e

if [ "$exit_code" != 2 ]; then
    echo "Expected exit code 2, but actual exit code $exit_code"
    exit 1
fi
