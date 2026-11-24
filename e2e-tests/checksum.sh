#!/bin/bash

set -euxo pipefail

# shellcheck disable=SC1091
source lib/https-server.sh

# Copies of signing/signing-test.tar, each with a different checksum file
# next to it, or none.
serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'checksum-serve')"

digest() {
    "$(_https_server_python)" -I -S -c \
        'import hashlib, sys; print(hashlib.new(sys.argv[1], open(sys.argv[2], "rb").read()).hexdigest())' \
        "$@"
}

for name in sha256 sha512 tampered none other; do
    cp signing/signing-test.tar "$serve_dir/$name.tar"
done
sha256="$(digest sha256 signing/signing-test.tar)"
sha512="$(digest sha512 signing/signing-test.tar)"

# As `sha256sum` writes it.
printf '%s  sha256.tar\n' "$sha256" >"$serve_dir/sha256.tar.sha256"
# As `sha512sum --tag` writes it, next to a wrong sha256 one: sha512 is
# preferred.
printf 'SHA512 (sha512.tar) = %s\n' "$sha512" >"$serve_dir/sha512.tar.sha512"
printf '%s\n' "$(printf '0%.0s' $(seq 64))" >"$serve_dir/sha512.tar.sha256"
# The package was changed after its checksum was published.
printf '%s\n' "$sha256" >"$serve_dir/tampered.tar.sha256"
printf 'tampered' >>"$serve_dir/tampered.tar"
# The checksum of some other file.
printf '%s  something-else.tar\n' "$sha256" >"$serve_dir/other.tar.sha256"

start_https_server "$serve_dir"

log="$serve_dir/log"

install() {
    "$1" binstall --force --manifest-path manifests/checksum-Cargo.toml --no-confirm \
        --strategies crate-meta-data \
        --pkg-url "$HTTPS_SERVER_URL/$2" "${@:3}" signing-test 2>&1 | tee "$log"
    return "${PIPESTATUS[0]}"
}

install "$1" sha256.tar
grep -F "Verified sha256 checksum of package 'signing-test' from $HTTPS_SERVER_URL/sha256.tar.sha256" "$log"

install "$1" sha512.tar
grep -F "Verified sha512 checksum of package 'signing-test' from $HTTPS_SERVER_URL/sha512.tar.sha512" "$log"

set +e
install "$1" tampered.tar
exit_code="$?"
set -e
if [ "$exit_code" != 94 ]; then
    echo "Expected installing tampered.tar to fail with exit code 94, got $exit_code"
    exit 1
fi
grep -F "sha256 checksum of $HTTPS_SERVER_URL/tampered.tar is" "$log"
grep -F "but $HTTPS_SERVER_URL/tampered.tar.sha256 says it is $sha256" "$log"

# --skip-signatures is the way out of a wrong checksum file, and doesn't
# look for one.
: >"$HTTPS_SERVER_LOG"
install "$1" tampered.tar --skip-signatures
if grep -F '.sha' "$HTTPS_SERVER_LOG"; then
    echo "--skip-signatures should not look for checksum files"
    exit 1
fi

# Without a checksum file, the package is installed as before, after asking
# for the two most common ones.
: >"$HTTPS_SERVER_LOG"
install "$1" none.tar
if grep -F 'Verified' "$log"; then
    echo "none.tar has no checksum to verify"
    exit 1
fi
grep -Fx 'GET /none.tar.sha512' "$HTTPS_SERVER_LOG"
grep -Fx 'GET /none.tar.sha256' "$HTTPS_SERVER_LOG"

# A checksum file that doesn't list the package is ignored with a warning.
install "$1" other.tar
grep -F "Ignoring checksum file $HTTPS_SERVER_URL/other.tar.sha256: it has no checksum for the package" "$log"
