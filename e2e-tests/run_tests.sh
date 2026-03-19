#!/bin/bash

set -euo pipefail

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

{
    flock 200 || echo "Flock not supported"
    
    cat "$output"
    if [ "$exit_status" -ne 0 ]; then
        echo "$1.sh failed"
    fi
} 200>"/tmp/$(basename "$0")-output.lock"

exit "$exit_status"
