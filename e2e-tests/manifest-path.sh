#!/bin/bash

set -euxo pipefail

# Install binaries using `--manifest-path`
# Also test default github template
"$1" binstall --force --manifest-path "manifests/github-test-Cargo.toml" --no-confirm cargo-binstall

# Test that the installed binaries can be run
cargo binstall --help >/dev/null

cargo_binstall_version="$(cargo binstall -V)"
echo "$cargo_binstall_version"

[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]

cat "$CARGO_HOME/.crates.toml"
grep -F "cargo-binstall 0.12.0 (path+file://manifests/github-test-Cargo.toml)" <"$CARGO_HOME/.crates.toml"

# Test default github template with a `{ name }-v{ version }` tag,
# as release-plz and cargo-release make in a workspace.
"$1" binstall --force --manifest-path "manifests/github-name-tag-test-Cargo.toml" --no-confirm --strategies crate-meta-data release-plz

release_plz_version="$(release-plz --version)"
echo "$release_plz_version"

[ "$release_plz_version" = "release-plz 0.4.3" ]

# Test that `--manifest-path` can handle relative path without parent well
exe="$(realpath "$1")"
cd manifests
"$exe" binstall --force --manifest-path github-test-Cargo.toml --no-confirm cargo-binstall

cargo_binstall_version="$(cargo binstall -V)"
echo "$cargo_binstall_version"

[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]
