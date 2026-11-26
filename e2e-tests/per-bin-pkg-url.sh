#!/bin/bash

set -euxo pipefail

# Each binary of the crate is downloaded from its own package, because
# pkg-url contains `{ bin }`.

# shellcheck disable=SC1091
source lib/https-server.sh

if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    ext=.exe
else
    ext=
fi

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'per-bin-serve')"
mkdir "$serve_dir/bin" "$serve_dir/gz" "$serve_dir/tgz" "$serve_dir/pkg"

for bin in per-bin-a per-bin-b; do
    # The binaries only need to be told apart, not run.
    printf '%s\n' "$bin" >"$serve_dir/bin/$bin$ext"

    # A single binary compressed without a tarball.
    gzip -c "$serve_dir/bin/$bin$ext" >"$serve_dir/gz/$bin.gz"

    # A tarball with the binary in a dir named after it rather than after
    # the crate, the only dir at its top, which bin-dir is inferred to.
    mkdir "$serve_dir/pkg/$bin-v0.1.0"
    cp "$serve_dir/bin/$bin$ext" "$serve_dir/pkg/$bin-v0.1.0/"
    tar -C "$serve_dir/pkg" -czf "$serve_dir/tgz/$bin.tgz" "$bin-v0.1.0"
done

start_https_server "$serve_dir"

install() {
    "$1" binstall --force --manifest-path manifests/per-bin-pkg-url-Cargo.toml --no-confirm \
        --strategies crate-meta-data --pkg-url "$HTTPS_SERVER_URL/$2" per-bin-test "${@:3}"
}

# check_installed BIN...: each BIN is installed, from its own package.
check_installed() {
    for bin in "$@"; do
        grep -qx "$bin" "$CARGO_HOME/bin/$bin$ext"
    done
}

check_not_installed() {
    for bin in "$@"; do
        if [ -e "$CARGO_HOME/bin/$bin$ext" ]; then
            echo "$bin should not be installed"
            exit 1
        fi
    done
}

# Raw binaries: both required bins are installed, the optional one whose
# package is missing is skipped.
install "$1" 'bin/{ bin }{ binary-ext }' --pkg-fmt bin
check_installed per-bin-a per-bin-b
check_not_installed per-bin-extra
grep -qF "GET /bin/per-bin-extra$ext" "$HTTPS_SERVER_LOG"
cargo uninstall per-bin-test
check_not_installed per-bin-a per-bin-b

# Only the package of the requested binary is looked for.
: >"$HTTPS_SERVER_LOG"
install "$1" 'bin/{ bin }{ binary-ext }' --pkg-fmt bin --bin per-bin-b
check_installed per-bin-b
check_not_installed per-bin-a
if grep -qF /bin/per-bin-a "$HTTPS_SERVER_LOG"; then
    echo "The package of per-bin-a was looked for, though only per-bin-b was asked for"
    exit 1
fi
cargo uninstall per-bin-test

# Single binaries compressed without a tarball, with the format guessed from
# the `.gz` in pkg-url.
install "$1" 'gz/{ bin }.gz'
check_installed per-bin-a per-bin-b
cargo uninstall per-bin-test

# Tarballs, with bin-dir inferred from their only top dir.
install "$1" 'tgz/{ bin }.tgz' --pkg-fmt tgz
check_installed per-bin-a per-bin-b
cargo uninstall per-bin-test

# The package of a required binary is missing, so there is nothing to install.
set +e
install "$1" 'bin/{ bin }-missing' --pkg-fmt bin
exit_code="$?"
set -e

if [ "$exit_code" != 94 ]; then
    echo "Expected exit code 94, but actual exit code $exit_code"
    exit 1
fi
check_not_installed per-bin-a per-bin-b
