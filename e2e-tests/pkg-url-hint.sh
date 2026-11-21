#!/bin/bash

set -euxo pipefail

# Checks that binstall records which default pkg-url a package was found with
# and tries it first when updating, falling back to all of them if it misses.

unset CARGO_INSTALL_ROOT

manifest="$CARGO_HOME/binstall/crates-v1.json"
args=(-y --force --strategies crate-meta-data --maximum-resolution-timeout 300 --log-level debug)

# cargo-watch has no binstall metadata, so it is found with a default pkg-url.
"$1" binstall "${args[@]}" cargo-watch@8.4.0
cargo-watch -V | grep -q 8.4.0
grep -q '"pkg_url_hint":{"name":"cargo-watch","pkg_url":"{ repo }' "$manifest"

log="$(mktemp)"

"$1" binstall "${args[@]}" cargo-watch@8.4.1 2>&1 | tee "$log"
cargo-watch -V | grep -q 8.4.1
grep -q "Found package with the pkg-url of the last install" "$log"

# A hint that is not one of the default pkg-url is ignored.
sed -i.bak 's/"pkg_url":"[^"]*"/"pkg_url":"https:\/\/example.com\/{ name }"/' "$manifest"
"$1" binstall "${args[@]}" cargo-watch@8.4.0 2>&1 | tee "$log"
cargo-watch -V | grep -q 8.4.0
if grep -q "Found package with the pkg-url of the last install" "$log"; then
    echo "binstall used a pkg-url hint that is not a default pkg-url"
    exit 1
fi
grep -q "did not match, trying all default pkg-url" "$log"
grep -q '"pkg_url":"{ repo }' "$manifest"
