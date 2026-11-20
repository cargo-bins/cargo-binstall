#!/bin/bash

set -euxo pipefail

# Copy it to bin to test use of env var `CARGO`
cp "$1" "$CARGO_HOME/bin/"

# Install with an alias given on the command line
cargo binstall --no-confirm --disable-strategies compile --aliases cargo-watch:cw cargo-watch@8.4.0

# The alias runs the installed binary
cw --version

# Uninstalling removes the alias along with the binary
cargo uninstall cargo-watch

# bash remembers where it found `cw` above, so forget it before checking.
hash -r
if command -v cw >/dev/null 2>&1; then
    echo "cw is still installed after uninstalling cargo-watch"
    exit 1
fi

# An alias that is not a plain file name is rejected
set +e

cargo binstall --no-confirm --disable-strategies compile --aliases 'cargo-watch:../cw' cargo-watch@8.4.0
exit_code="$?"

set -e

if [ "$exit_code" = 0 ]; then
    echo "Expected an alias that escapes the install dir to be rejected"
    exit 1
fi

if [ -e "$CARGO_HOME/cw" ]; then
    echo "An alias was created outside of the install dir"
    exit 1
fi

# Verify that --aliases cannot be combined with multiple packages
set +e

cargo binstall --no-confirm --aliases cargo-watch:cw cargo-watch cargo-binstall
exit_code="$?"

set -e

if [ "$exit_code" != 2 ]; then
    echo "Expected exit code 2, but actual exit code $exit_code"
    exit 1
fi

# `cargo install` can't create aliases, so asking for them on the command
# line when installing from source is an error.
set +e

cargo binstall --no-confirm --strategies compile --aliases cargo-watch:cw cargo-watch@8.4.0
exit_code="$?"

set -e

if [ "$exit_code" != 101 ]; then
    echo "Expected exit code 101, but actual exit code $exit_code"
    exit 1
fi
