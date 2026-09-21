#!/bin/bash

set -euxo pipefail

# Test for the features flag
#
# cargo-about's `cargo-about` binary is gated behind the `cli` feature, so
# compiling it from source without passing `--features=cli` through to
# `cargo install` silently produces a package with no binaries.
"$1" binstall --no-confirm \
    cargo-about --version 0.9.2 --locked --force \
    --strategies compile \
    --features cli

# Verify that the binary was installed and is executable
if ! command -v cargo-about >/dev/null 2>&1; then
  echo "cargo-about was not installed"
  exit 1
fi

# Run the binary to check it works
cargo-about --version

# Verify that --features cannot be combined with multiple packages
set +e

"$1" binstall --no-confirm cargo-binstall cargo-about --strategies compile --features cli
exit_code="$?"

set -e

if [ "$exit_code" != 2 ]; then
    echo "Expected exit code 2, but actual exit code $exit_code"
    exit 1
fi
