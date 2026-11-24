#!/bin/bash

set -euxo pipefail

# shellcheck disable=SC1091
source lib/https-server.sh

python="$(command -v python3 || command -v python)"

# Compressed zeros: "small" (1 MiB) is well under the cap, "bomb" (600 MiB)
# shrinks to about 600 KiB, so it goes over the 512 MiB floor while staying far
# past 20 times its compressed size.
serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'unpack-limit-serve')"
"$python" lib/make-bomb.py "$serve_dir" small $((1 << 20))
"$python" lib/make-bomb.py "$serve_dir" bomb $((600 << 20))
ls -l "$serve_dir"

start_https_server "$serve_dir"

root="$(mktemp -d 2>/dev/null || mktemp -d -t 'unpack-limit-root')"
log="$(mktemp)"

# install EXE FILE ARGS...: install from FILE, in the format its extension
# names.
install() {
    "$1" binstall "${@:3}" --force --manifest-path manifests/unpack-limit-Cargo.toml \
        --no-confirm --disable-strategies compile --strategies crate-meta-data \
        --root "$root" --pkg-url "$HTTPS_SERVER_URL/$2" --pkg-fmt "${2##*.}" \
        unpack-limit-test
}

for ext in gz zip; do
    install "$1" "small.$ext"
    ls "$root/bin/unpack-limit-test"*
    rm -rf "${root:?}"/*

    set +e
    install "$1" "bomb.$ext" 2>&1 | tee "$log"
    exit_code="${PIPESTATUS[0]}"
    set -e

    if [ "$exit_code" = 0 ]; then
        echo "Expected installing bomb.$ext to fail"
        exit 1
    fi
    grep -F "possible decompression bomb" "$log"

    if compgen -G "$root/bin/unpack-limit-test*"; then
        echo "bomb.$ext was installed"
        exit 1
    fi
done

# The cap can be raised from the command line or the environment.
install "$1" bomb.gz --max-unpack-size 1GiB
ls "$root/bin/unpack-limit-test"*
rm -rf "${root:?}"/*

BINSTALL_MAX_UNPACK_RATIO=2000 install "$1" bomb.zip
ls "$root/bin/unpack-limit-test"*
