#!/bin/bash

set -euxo pipefail

# Each binary of the crate is downloaded from its own package, because
# pkg-url contains `{ bin }`.

echo Generate tls cert

CERT_DIR=$(mktemp -d 2>/dev/null || mktemp -d -t 'cert-dir')
export CERT_DIR

openssl req -newkey rsa:4096 -x509 -sha256 -days 1 -nodes -out "$CERT_DIR/"ca.pem -keyout "$CERT_DIR/"ca.key -subj '//C=UT/CN=ca.localhost'
openssl req -new -newkey rsa:4096 -sha256 -nodes -out "$CERT_DIR/"server.csr -keyout "$CERT_DIR/"server.key -subj '//C=UT/CN=localhost'
openssl x509 -req -in "$CERT_DIR/"server.csr -CA "$CERT_DIR/"ca.pem -CAkey "$CERT_DIR/"ca.key -CAcreateserial -out "$CERT_DIR/"server.pem -days 1 -sha256 -extfile signing/server.ext

echo Generate packages

SERVE_DIR=$(mktemp -d 2>/dev/null || mktemp -d -t 'serve-dir')
mkdir "$SERVE_DIR/bin" "$SERVE_DIR/tgz"

for bin in per-bin-a per-bin-b; do
    # Raw binaries, for pkg-fmt = "bin"
    printf '#!/bin/sh\necho %s\n' "$bin" > "$SERVE_DIR/bin/$bin"
    chmod +x "$SERVE_DIR/bin/$bin"

    # Archives with the binary in a dir named after it, to test that
    # bin-dir is inferred from the binary name.
    mkdir -p "$SERVE_DIR/pkg/$bin-v0.1.0"
    cp "$SERVE_DIR/bin/$bin" "$SERVE_DIR/pkg/$bin-v0.1.0/"
    tar -C "$SERVE_DIR/pkg" -czf "$SERVE_DIR/tgz/$bin.tgz" "$bin-v0.1.0"
done

python3 -c '
import http.server, os, ssl, sys
from functools import partial
cert_dir = os.environ["CERT_DIR"]
handler = partial(http.server.SimpleHTTPRequestHandler, directory=sys.argv[1])
httpd = http.server.HTTPServer(("", 4444), handler)
ctx = ssl.SSLContext(protocol=ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(certfile=f"{cert_dir}/server.pem", keyfile=f"{cert_dir}/server.key")
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
' "$SERVE_DIR" &
server_pid=$!
trap 'kill $server_pid' ERR INT TERM

export BINSTALL_HTTPS_ROOT_CERTS="$CERT_DIR/ca.pem"

counter=0
while ! curl --cacert "$BINSTALL_HTTPS_ROOT_CERTS" -fsSL https://localhost:4444/bin/per-bin-a >/dev/null; do
    counter=$(( counter + 1 ))
    if [ "$counter" = "20" ]; then
        echo Failed to connect to https server
        exit 1
    fi
    sleep 1
done

manifest=manifests/per-bin-pkg-url-Cargo.toml

check_installed() {
    hash -r
    for bin in "$@"; do
        [ "$("$bin")" = "$bin" ]
    done
}

check_not_installed() {
    hash -r
    for bin in "$@"; do
        if command -v "$bin" >/dev/null 2>&1; then
            echo "$bin should not be installed"
            exit 1
        fi
    done
}

# Raw binaries: both required bins are installed, the optional one whose
# package is missing is skipped.
"$1" binstall --force --manifest-path "$manifest" --no-confirm \
    --disable-strategies compile,quick-install per-bin-test
check_installed per-bin-a per-bin-b
check_not_installed per-bin-extra
cargo uninstall per-bin-test
check_not_installed per-bin-a per-bin-b

# Only the requested binary is downloaded
"$1" binstall --force --manifest-path "$manifest" --no-confirm \
    --disable-strategies compile,quick-install per-bin-test --bin per-bin-b
check_installed per-bin-b
check_not_installed per-bin-a
cargo uninstall per-bin-test

# Archives, with bin-dir inferred from the binary name
"$1" binstall --force --manifest-path "$manifest" --no-confirm \
    --disable-strategies compile,quick-install \
    --pkg-url 'https://localhost:4444/tgz/{ bin }.tgz' --pkg-fmt tgz per-bin-test
check_installed per-bin-a per-bin-b
cargo uninstall per-bin-test

# The package of a required binary is missing, so there is nothing to install
set +e
"$1" binstall --force --manifest-path "$manifest" --no-confirm \
    --disable-strategies compile,quick-install \
    --pkg-url 'https://localhost:4444/bin/{ bin }-missing' per-bin-test
exit_code="$?"
set -e

if [ "$exit_code" != 94 ]; then
    echo "Expected exit code 94, but actual exit code $exit_code"
    exit 1
fi
check_not_installed per-bin-a per-bin-b

kill $server_pid || true
