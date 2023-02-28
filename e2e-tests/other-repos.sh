#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

export CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export PATH="$CARGO_HOME/bin:$PATH"

# Test default GitLab pkg-url templates
#"./$1" binstall \
#    --force \
#    --manifest-path "manifests/gitlab-test-Cargo.toml" \
#    --no-confirm \
#    --disable-strategies compile \
#    cargo-binstall

# Test default BitBucket pkg-url templates
"./$1" binstall \
    --force \
    --manifest-path "manifests/bitbucket-test-Cargo.toml" \
    --no-confirm \
    --disable-strategies compile \
    cargo-binstall

# Test default Github pkg-url templates,
# with bin-dir provided
"./$1" binstall \
    --force \
    --manifest-path "manifests/github-test-Cargo2.toml" \
    --no-confirm \
    --disable-strategies compile \
    cargo-binstall
