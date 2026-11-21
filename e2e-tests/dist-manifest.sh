#!/bin/bash

set -euxo pipefail

# Releases made by dist (https://github.com/axodotdev/cargo-dist) come with a
# dist-manifest.json naming the archive for each target and its sha256: the
# package is found from it instead of probing every default pkg-url, and its
# checksum is verified.

unset CARGO_INSTALL_ROOT

manifest="$CARGO_HOME/binstall/crates-v1.json"
args=(-y --force --strategies crate-meta-data --log-level debug)
log="$(mktemp)"

# axolotlsay is dist's example app, and has no binstall metadata.
"$1" binstall "${args[@]}" axolotlsay@0.3.3 2>&1 | tee "$log"
axolotlsay --version | grep -F '0.3.3'

grep -F "Found package in dist-manifest.json" "$log"
grep -F "Verified sha256 checksum of package 'axolotlsay' from dist-manifest.json" "$log"
if grep -F "Checking for package at" "$log"; then
    echo "binstall probed default pkg-url despite the dist-manifest.json"
    exit 1
fi

# The manifest's url is recorded, so that updating only looks there.
grep -F '"pkg_url_hint":{"name":"axolotlsay","pkg_url":"{ repo }/releases/download/v{ version }/dist-manifest.json"' "$manifest"

"$1" binstall "${args[@]}" axolotlsay@0.3.2 2>&1 | tee "$log"
axolotlsay --version | grep -F '0.3.2'
grep -F "Found package in dist-manifest.json" "$log"
[ "$(grep -cF "Checking for dist-manifest.json at" "$log")" -eq 1 ]
