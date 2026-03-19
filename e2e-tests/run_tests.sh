#!/bin/bash

set -euo pipefail

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
cp "$2" "$CARGO_HOME/"

output="$(mktemp)"
echo "::group::$1" >> "$output"

cd e2e-tests
set +e
env -u RUSTFLAGS \
    -u CARGO_BUILD_TARGET \
    -u CARGO_INSTALL_ROOT \
    CARGO_HOME="$CARGO_HOME" \
    PATH="$CARGO_HOME/bin:$PATH" \
    bash "$1.sh" \
    "${@:3}" >> "$output" 2>&1
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
