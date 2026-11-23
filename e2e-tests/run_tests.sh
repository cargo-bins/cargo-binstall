#!/bin/bash

set -euo pipefail

CARGO_HOME="$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')"
mkdir -p "$CARGO_HOME/bin"

tempdir="$(mktemp -d 2>/dev/null || mktemp -d -t 'tempdir')"
cp "$2" "$tempdir/"

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
    "$tempdir/$(basename "$2")" \
    "${@:3}" >> "$output" 2>&1
exit_status="$?"
set -e

echo "::endgroup::" >> "$output"

print_output() {
    cat "$output"
    if [ "$exit_status" -ne 0 ]; then
        echo "$1.sh failed"
    fi
}

# The tests run in parallel: hold a lock while printing so that their outputs
# don't interleave.
lock="/tmp/$(basename "$0")-output.lock"
if command -v flock >/dev/null 2>&1; then
    {
        flock 200
        print_output "$1"
    } 200>"$lock"
else
    # Git Bash on Windows has no flock(1); mkdir is atomic, so use a directory
    # as the lock. Give up waiting after a minute so that a lock left behind by
    # a killed run only costs readable output, not a hang.
    locked=false
    for _ in $(seq 1 600); do
        if mkdir "$lock.d" 2>/dev/null; then
            locked=true
            trap 'rmdir "$lock.d"' EXIT
            break
        fi
        sleep 0.1
    done
    print_output "$1"
    if "$locked"; then
        rmdir "$lock.d"
        trap - EXIT
    fi
fi

exit "$exit_status"
