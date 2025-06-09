#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

# Install a specific binary (e.g., ripgrep)
"./$1" binstall --no-confirm ripgrep --bin rg

# Verify that the binary was installed and is executable
if ! command -v rg >/dev/null 2>&1; then
  echo "rg was not installed"
  exit 1
fi

# Run the binary to check it works
rg --version
