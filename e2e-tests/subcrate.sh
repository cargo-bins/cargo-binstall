#!/bin/bash

set -euxo pipefail

# Copy it to bin to test use of env var `CARGO`
cp "$1" "$CARGO_HOME/bin"

# cargo-audit
cargo binstall --no-confirm cargo-audit@0.18.3 --strategies crate-meta-data

cargo_audit_version="$(cargo audit --version)"
echo "$cargo_audit_version"

[ "$cargo_audit_version" = "cargo-audit 0.18.3" ]
