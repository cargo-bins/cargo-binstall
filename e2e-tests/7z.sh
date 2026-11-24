#!/bin/bash

set -euxo pipefail

# Install from a 7z archive made by 7-Zip 23.01 (`7z a -snl`, so solid LZMA2
# with a unix symlink and an empty directory in it):
#
#     signing-test-7z/
#         empty-dir/
#         signing-test
#         signing-test.exe
#         signing-test-link -> signing-test

# shellcheck disable=SC1091
source lib/https-server.sh

start_https_server 7z

root="$(mktemp -d 2>/dev/null || mktemp -d -t '7z-root')"
"$1" binstall --force --manifest-path manifests/signing-Cargo.toml --no-confirm \
    --strategies crate-meta-data --skip-signatures \
    --root "$root" \
    --pkg-url "$HTTPS_SERVER_URL/signing-test.7z" \
    --pkg-fmt 7z \
    --bin-dir "signing-test-7z/{ bin }{ binary-ext }" \
    signing-test

test -f "$root/bin/signing-test" || test -f "$root/bin/signing-test.exe"
grep -E "^GET /signing-test\.7z" "$HTTPS_SERVER_LOG"
