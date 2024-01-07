#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

#  - `b3sum@<=1.3.3` would test `fetch_crate_cratesio_version_matched` ability
#    to find versions matching <= 1.3.3
#  - `cargo-quickinstall` would test `fetch_crate_cratesio_version_matched` ability
#    to find latest stable version.
crates="b3sum@<=1.3.3 cargo-release@0.24.9 cargo-binstall@0.20.1 cargo-watch@8.4.0 miniserve@0.23.0 sccache@0.3.3 cargo-quickinstall"

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
othertmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-test')
export PATH="$CARGO_HOME/bin:$othertmpdir/bin:$PATH"

mkdir -p "$othertmpdir/bin"
# Copy it to bin to test use of env var `CARGO`
cp "./$1" "$othertmpdir/bin/"

# Install binaries using cargo-binstall
# shellcheck disable=SC2086
cargo binstall --no-confirm $crates

rm -r "$othertmpdir"

# Test that the installed binaries can be run
b3sum_version="$(b3sum --version)"
echo "$b3sum_version"

[ "$b3sum_version" = "b3sum 1.3.3" ]

cargo_release_version="$(cargo-release release --version)"
echo "$cargo_release_version"

[ "$cargo_release_version" = "cargo-release 0.24.9" ]

cargo binstall --help >/dev/null

cargo_binstall_version="$(cargo-binstall -V)"
echo "cargo-binstall version $cargo_binstall_version"

[ "$cargo_binstall_version" = "0.20.1" ]

cargo_watch_version="$(cargo watch -V)"
echo "$cargo_watch_version"

[ "$cargo_watch_version" = "cargo-watch 8.4.0" ]

miniserve_version="$(miniserve -V)"
echo "$miniserve_version"

[ "$miniserve_version" = "miniserve 0.23.0" ]

cargo-quickinstall -V
