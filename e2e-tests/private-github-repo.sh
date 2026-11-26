#!/bin/bash

set -euxo pipefail

# Install binaries using `--manifest-path`
# Also test default github template
"$1" binstall --force --manifest-path "manifests/private-github-repo-test-Cargo.toml" --no-confirm cargo-binstall --strategies crate-meta-data

# Test that the installed binaries can be run
cargo binstall --help >/dev/null

cargo_binstall_version="$(cargo binstall -V)"
echo "$cargo_binstall_version"

[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]

# The crate's repository is private, but `pkg-url` points into a public one:
# its candidates are checked there as public ones, with github.com's asset
# list and a GET, not with the API.
log="$(mktemp 2>/dev/null || mktemp -t 'private-github-repo')"
"$1" binstall --force --manifest-path "manifests/private-github-repo-test-Cargo.toml" --no-confirm cargo-binstall --strategies crate-meta-data \
    --pkg-url "https://github.com/cargo-bins/cargo-binstall/releases/download/v{ version }/{ name }-{ target }.{ archive-format }" \
    --bin-dir "{ bin }{ binary-ext }" \
    --log-level debug 2>&1 | tee "$log"

if grep "Checking for package with the API" "$log"; then
    echo "A candidate in the public repository was checked with the API"
    exit 1
fi

cargo_binstall_version="$(cargo binstall -V)"
echo "$cargo_binstall_version"

[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]
