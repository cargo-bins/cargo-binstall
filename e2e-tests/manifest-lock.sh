#!/bin/bash

set -euxo pipefail

# Checks that binstall doesn't hold the manifest lock while resolving, only
# while installing, so concurrent runs (e.g. from cargo-update) don't queue
# behind each other's resolution.

if ! command -v flock >/dev/null; then
    echo "flock(1) not available, skipping"
    exit 0
fi

unset CARGO_INSTALL_ROOT

crates_toml="$CARGO_HOME/.crates.toml"
log="$(mktemp)"

"$1" binstall -y cargo-binstall@0.20.1 >"$log" 2>&1 &
pid="$!"

wait_for() {
    for _ in $(seq 1200); do
        if grep -q "$1" "$log"; then
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            break
        fi
        sleep 0.1
    done
    grep -q "$1" "$log"
}

wait_for "Resolving package"

# Resolution has started, so the lock must be free: take it as another process
# about to install would.
exec 9>>"$crates_toml"
flock -w 10 9

# binstall now has to wait for us before installing.
wait_for "Blocking waiting for file lock"
if grep -q "Installing binaries" "$log"; then
    cat "$log"
    echo "binstall installed while another process held the manifest lock"
    exit 1
fi

flock -u 9
exec 9>&-

wait "$pid"
cat "$log"

grep -q "Installing binaries" "$log"
cargo-binstall --help >/dev/null
grep -q "cargo-binstall 0.20.1" "$crates_toml"
