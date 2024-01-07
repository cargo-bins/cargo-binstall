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


## Test --continue-on-failure
set +e
cargo binstall --no-confirm --continue-on-failure cargo-watch@8.4.0 non-existent-clippy
exit_code="$?"

set -e

if [ "$exit_code" != 76 ]; then
    echo "Expected exit code 94, but actual exit code $exit_code"
    exit 1
fi


cargo_watch_version="$(cargo watch -V)"
echo "$cargo_watch_version"

[ "$cargo_watch_version" = "cargo-watch 8.4.0" ]
