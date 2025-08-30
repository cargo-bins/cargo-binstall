#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

# Install a specific binary, ensuring we don't fallback to source.
"./$1" binstall --no-confirm \
    taplo-cli --bin taplo \
    --pkg-url "{ repo }/releases/download/{ version }/taplo-{ target-family }-{ target-arch }.gz" \
    --bin-dir "taplo-{ target-family }-{ target-arch }/{ bin }{ binary-ext }" \
    --pkg-fmt="tgz"

# Verify that the binary was installed and is executable
if ! command -v taplo >/dev/null 2>&1; then
  echo "taplo was not installed"
  exit 1
fi

# Run the binary to check it works
taplo --version

# Install a specific binary, but always compile from source.
"./$1" binstall --no-confirm ripgrep --bin rg --strategies compile

# Verify that the binary was installed and is executable
if ! command -v rg >/dev/null 2>&1; then
  echo "rg was not installed"
  exit 1
fi

# Run the binary to check it works
rg --version
