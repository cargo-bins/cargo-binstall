#!/bin/bash

set -euxo pipefail

# 1. Initial empty state
"$1" binstall --list | grep "No installed crates found."
"$1" binstall --prune | grep "Everything is clean. No stale binstall records found."

# 2. Install a binary via binstall
"$1" binstall -y cargo-watch@8.4.0

# 3. Test --list (human-readable table and json output)
"$1" binstall --list | grep "cargo-watch" | grep "cargo binstall"
"$1" binstall --list --json-output | grep '"name": "cargo-watch"'

# 4. Prune when up-to-date (no-op)
"$1" binstall --prune | grep "Everything is clean. No stale binstall records found."

# 5. Uninstall from cargo to create a stale binstall manifest record
cargo uninstall cargo-watch

# 6. Verify --prune detects and removes the stale uninstalled crate
"$1" binstall --prune | grep "Pruned stale record: cargo-watch"

# 7. Second prune confirms clean state
"$1" binstall --prune | grep "Everything is clean. No stale binstall records found."
"$1" binstall --list | grep "No installed crates found."
