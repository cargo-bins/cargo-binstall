#!/bin/bash

set -euxo pipefail

output="$(mktemp)"
binary="$(mktemp)"

cp "$2" "$binary"
chmod 555 "$binary"

echo "::group::$1" >> "$output"

cd e2e-tests
set +e
env -u RUSTFLAGS \
    -u CARGO_BUILD_TARGET \
    bash "$1.sh" \
    "$binary" "${@:3}" >> "$output" 2>&1
exit_status="$?"
set -e

echo "::endgroup::" >> "$output"

cat "$output"
exit "$exit_status"
