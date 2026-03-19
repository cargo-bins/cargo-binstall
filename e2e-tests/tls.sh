#!/bin/bash

set -euxo pipefail

"$1" binstall \
    --force \
    --min-tls-version "${2:-1.3}" \
    --no-confirm \
    cargo-binstall@0.20.1
# Test that the installed binaries can be run
cargo binstall --help >/dev/null
