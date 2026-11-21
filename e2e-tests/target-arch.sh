#!/bin/bash

set -euxo pipefail

# Release assets named `arm64` or `amd64` rather than Rust's `aarch64` or
# `x86_64`: when pkg-url uses `{ target-arch }`, cargo-binstall also tries
# those aliases, and renders bin-dir with the one it found.

# shellcheck disable=SC1091
source lib/https-server.sh

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'target-arch')"

# signing/signing-test.tar's binary, under a directory named after the alias,
# so that the install only succeeds if bin-dir uses the alias too.
for arch in arm64 amd64; do
    mkdir "$serve_dir/signing-test-$arch"
    tar -xf signing/signing-test.tar -C "$serve_dir/signing-test-$arch" signing-test
    tar -cf "$serve_dir/signing-test-$arch.tar" -C "$serve_dir" "signing-test-$arch"
    rm -r "${serve_dir:?}/signing-test-$arch"
done

start_https_server "$serve_dir"

requests_for() {
    grep -cE " /$1( |\$)" "$HTTPS_SERVER_LOG" || true
}

for target in aarch64-unknown-linux-gnu:arm64 x86_64-unknown-linux-gnu:amd64; do
    IFS=: read -r target alias <<<"$target"

    root="$(mktemp -d 2>/dev/null || mktemp -d -t 'target-arch-root')"
    "$1" binstall --force --manifest-path manifests/signing-Cargo.toml --no-confirm \
        --strategies crate-meta-data --skip-signatures --targets "$target" \
        --root "$root" \
        --pkg-url "$HTTPS_SERVER_URL/signing-test-{ target-arch }.tar" \
        --bin-dir "signing-test-{ target-arch }/{ bin }" \
        signing-test

    # Found under the alias. Rust's name is checked at the same time, so
    # whether its (failing) request was made before the alias won is racy.
    [ "$(requests_for "signing-test-$alias.tar")" -ge 1 ]
    test -f "$root/bin/signing-test"
done

cat "$HTTPS_SERVER_LOG"
