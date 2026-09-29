#!/bin/bash

set -euxo pipefail

# 1. Install a binary via binstall
"$1" binstall -y cargo-watch@8.4.0
"$1" binstall --list | grep "cargo-watch" | grep "cargo binstall"
grep "cargo-watch" "${CARGO_HOME:-$HOME/.cargo}/binstall/crates-v1.json"

# 2. Uninstall cargo-watch via cargo, leaving a stale entry in binstall/crates-v1.json
cargo uninstall cargo-watch
grep "cargo-watch" "${CARGO_HOME:-$HOME/.cargo}/binstall/crates-v1.json"

# 3. Installing another crate via binstall should automatically prune the stale record
"$1" binstall -y cargo-binstall@0.20.1

# 4. Verify cargo-watch was auto-pruned from crates-v1.json without needing manual prune
if grep "cargo-watch" "${CARGO_HOME:-$HOME/.cargo}/binstall/crates-v1.json"; then
    exit 1
fi
"$1" binstall --list | grep "cargo-binstall" | grep "cargo binstall"
if "$1" binstall --list | grep "cargo-watch"; then
    exit 1
fi

# 5. Confirm that manual prune reports clean because auto-prune already did the cleanup
"$1" binstall --prune | grep "Everything is clean. No stale binstall records found."
