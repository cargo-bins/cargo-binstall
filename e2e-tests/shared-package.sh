#!/bin/bash

set -euxo pipefail

# Crates installed together that are in the same package, such as nushell
# and its plugins, download and extract it once.
#
# `--pkg-url` points both crates at one local archive, which has the
# binaries of both.

# shellcheck disable=SC1091
source lib/https-server.sh

unset CARGO_INSTALL_ROOT

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'shared-package')"
pkg_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'shared-package-pkg')"
for bin in b3sum cargo-watch; do
    # `.exe` too, so that `{ binary-ext }` finds it on Windows.
    for file in "$bin" "$bin.exe"; do
        printf '#!/bin/sh\necho %s\n' "$bin" >"$pkg_dir/$file"
        chmod +x "$pkg_dir/$file"
    done
done
# Random, so that it doesn't compress: much more than the socket buffers
# hold, so that the server only logs it as sent if it was read.
head -c 16777216 /dev/urandom >"$pkg_dir/padding"
tar -czf "$serve_dir/shared.tgz" -C "$pkg_dir" .

start_https_server "$serve_dir"

# One target, so that the number of requests doesn't depend on how many
# targets the host has.
target="$("$1" -vV | sed -n 's/^build-target: //p')"

"$1" binstall --no-confirm --strategies crate-meta-data --targets "$target" \
    --pkg-url "$HTTPS_SERVER_URL/shared.tgz" --pkg-fmt tgz \
    --bin-dir "{ bin }{ binary-ext }" \
    b3sum@1.5.0 cargo-watch@8.4.0

cat "$HTTPS_SERVER_LOG" "$HTTPS_SERVER_SENT_LOG"

for bin in b3sum cargo-watch; do
    if [ ! -e "$CARGO_HOME/bin/$bin" ] && [ ! -e "$CARGO_HOME/bin/$bin.exe" ]; then
        echo "$bin was not installed"
        exit 1
    fi
done

# Each crate checks that the package exists with a GET request, and the one
# that downloads it reads that response, the other one doesn't.
downloads="$(grep -c '^/shared.tgz$' "$HTTPS_SERVER_SENT_LOG" || true)"
if [ "$downloads" != 1 ]; then
    echo "Expected shared.tgz to be downloaded once, got $downloads downloads"
    exit 1
fi

# With a `bin-dir` that is the same file for both crates, installing it
# leaves it where it is, so the crate that installs second still finds it.
: >"$HTTPS_SERVER_LOG"
: >"$HTTPS_SERVER_SENT_LOG"
rm -f "$CARGO_HOME"/bin/b3sum* "$CARGO_HOME"/bin/cargo-watch*

"$1" binstall --no-confirm --force --strategies crate-meta-data --targets "$target" \
    --pkg-url "$HTTPS_SERVER_URL/shared.tgz" --pkg-fmt tgz \
    --bin-dir "b3sum{ binary-ext }" \
    b3sum@1.5.0 cargo-watch@8.4.0

cat "$HTTPS_SERVER_LOG" "$HTTPS_SERVER_SENT_LOG"

for bin in b3sum cargo-watch; do
    if [ ! -e "$CARGO_HOME/bin/$bin" ] && [ ! -e "$CARGO_HOME/bin/$bin.exe" ]; then
        echo "$bin was not installed"
        exit 1
    fi
done

downloads="$(grep -c '^/shared.tgz$' "$HTTPS_SERVER_SENT_LOG" || true)"
if [ "$downloads" != 1 ]; then
    echo "Expected shared.tgz to be downloaded once, got $downloads downloads"
    exit 1
fi
