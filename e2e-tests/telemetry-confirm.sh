#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME

printf 'yes\nyes\n' | "./$1" binstall cargo-quickinstall

echo
ls -lsha "$CARGO_HOME"
grep "consent_asked = true" "$CARGO_HOME/binstall.toml"
echo

printf 'yes\n' | "./$1" binstall --force cargo-quickinstall 2>&1 | tee /tmp/out
grep "Opt in to telemetry?" /tmp/out && exit 1 || exit 0