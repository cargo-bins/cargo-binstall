#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

# Test default GitLab pkg-url templates
#"./$1" binstall \
#    --force \
#    --manifest-path "manifests/gitlab-test-Cargo.toml" \
#    --no-confirm \
#    --disable-strategies compile \
#    cargo-binstall

# temporarily disable bitbucket testing as bitbucket is down
## Test default BitBucket pkg-url templates
#"./$1" binstall \
#    --force \
#    --manifest-path "manifests/bitbucket-test-Cargo.toml" \
#    --no-confirm \
#    --disable-strategies compile \
#    cargo-binstall
#
## Test that the installed binaries can be run
#cargo binstall --help >/dev/null
#
#cargo_binstall_version="$(cargo binstall -V)"
#echo "$cargo_binstall_version"
#
#[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]

# Do not test Codeberg, it is donation funded and shouldn't be burdened with
# unnecessary traffic.

# Test default Github pkg-url templates,
# with bin-dir provided
"./$1" binstall \
    --force \
    --manifest-path "manifests/github-test-Cargo2.toml" \
    --no-confirm \
    --disable-strategies compile \
    cargo-binstall

# Test that the installed binaries can be run
cargo binstall --help >/dev/null

cargo_binstall_version="$(cargo binstall -V)"
echo "$cargo_binstall_version"

[ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]
