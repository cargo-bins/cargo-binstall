#!/bin/bash

set -euxo pipefail

# shellcheck disable=SC1091
source lib/https-server.sh

# signing/signing-test.tar, signed by signing/minisign.key, plus two broken
# copies of it: one whose signature no longer matches, one without a
# signature at all.
serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'signing-serve')"
cp signing/signing-test.tar signing/signing-test.tar.sig "$serve_dir/"
cp signing/signing-test.tar "$serve_dir/tampered.tar"
printf 'tampered' >>"$serve_dir/tampered.tar"
cp signing/signing-test.tar.sig "$serve_dir/tampered.tar.sig"
cp signing/signing-test.tar "$serve_dir/unsigned.tar"
# The same package signed with `gpg --detach-sign` by the key in
# signing/openpgp.asc.
cp signing/signing-test.tar "$serve_dir/openpgp.tar"
cp signing/signing-test.tar.gpgsig "$serve_dir/openpgp.tar.sig"
cp signing/signing-test.tar "$serve_dir/openpgp-tampered.tar"
printf 'tampered' >>"$serve_dir/openpgp-tampered.tar"
cp signing/signing-test.tar.gpgsig "$serve_dir/openpgp-tampered.tar.sig"

start_https_server "$serve_dir"

manifest=manifests/signing-Cargo.toml

install() {
    "$1" binstall --force --manifest-path "$manifest" --no-confirm \
        --strategies crate-meta-data \
        --pkg-url "$HTTPS_SERVER_URL/$2" "${@:3}" signing-test
}

log="$serve_dir/log"

# expect_failure EXE REASON PKG ARGS...: the install must fail, because of
# REASON rather than anything else.
expect_failure() {
    set +e
    install "$1" "${@:3}" 2>&1 | tee "$log"
    exit_code="${PIPESTATUS[0]}"
    set -e

    if [ "$exit_code" != 94 ]; then
        echo "Expected installing $3 with ${*:4} to fail with exit code 94, got $exit_code"
        exit 1
    fi
    grep -F "$2" "$log"
}

install "$1" signing-test.tar
install "$1" signing-test.tar --only-signed
install "$1" signing-test.tar --skip-signatures

# The manifest has a signing section, so a bad signature is an error whether
# or not --only-signed is passed.
expect_failure "$1" "Failed to verify signature" tampered.tar
expect_failure "$1" "Failed to verify signature" tampered.tar --only-signed
install "$1" tampered.tar --skip-signatures

expect_failure "$1" "unsigned.tar.sig: HTTP status client error (404" unsigned.tar
expect_failure "$1" "unsigned.tar.sig: HTTP status client error (404" unsigned.tar --only-signed
install "$1" unsigned.tar --skip-signatures

manifest=manifests/signing-openpgp-Cargo.toml

install "$1" openpgp.tar 2>&1 | tee "$log"
grep -F "Verified signature for package 'signing-test': OpenPGP key" "$log"
install "$1" openpgp.tar --only-signed

expect_failure "$1" "OpenPGP signature does not match the package" openpgp-tampered.tar
# A minisign signature is not an OpenPGP signature.
expect_failure "$1" "Signature file is invalid" signing-test.tar

# from quick-install
#"$1" binstall --force --strategies quick-install --no-confirm --only-signed --target x86_64-unknown-linux-musl zellij@0.38.2
