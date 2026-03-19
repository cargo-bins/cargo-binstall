#!/bin/bash

set -euxo pipefail

# Copy it to bin to test use of env var `CARGO`
cp "$1" "$CARGO_HOME/bin/"

cargo binstall --no-confirm cargo-watch@8.4.0
cargo uninstall cargo-watch
