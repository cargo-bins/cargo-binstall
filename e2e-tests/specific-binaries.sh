#!/bin/bash

set -euxo pipefail

# Install a specific binary from a prebuilt release, ensuring we don't
# fallback to source.
#
# jj-cli also declares `fake-editor` and other test-only binaries.
"$1" binstall --no-confirm \
    jj-cli@0.18.0 --bin jj \
    --strategies crate-meta-data

# Verify that the binary was installed and is executable
if ! command -v jj >/dev/null 2>&1; then
  echo "jj was not installed"
  exit 1
fi

# Run the binary to check it works
jj --version

# Install a specific binary, but always compile from source.
"$1" binstall --no-confirm ripgrep --bin rg --strategies compile

# Verify that the binary was installed and is executable
if ! command -v rg >/dev/null 2>&1; then
  echo "rg was not installed"
  exit 1
fi

# Run the binary to check it works
rg --version
