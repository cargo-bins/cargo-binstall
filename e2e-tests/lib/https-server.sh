# shellcheck shell=bash
#
# Serve a local directory over HTTPS, so that an e2e test can install from
# fixtures instead of the internet, or serve responses the internet won't.
#
# Usage, from a test script (whose cwd is e2e-tests):
#
#     source lib/https-server.sh
#     start_https_server some/dir [routes.json]
#     "$1" binstall ... --pkg-url "$HTTPS_SERVER_URL/some-file.tar"
#
# routes.json, if given, lists responses to give for a path before serving
# it normally: error statuses, redirects or truncated bodies. See
# lib/https_server.py for its format.
#
# start_https_server makes a throwaway CA and a localhost leaf certificate
# signed by it (rustls rejects a CA certificate used as the server's own),
# starts the server on a free port and sets:
#
# - HTTPS_SERVER_URL, like https://localhost:12345 (no trailing slash)
# - BINSTALL_HTTPS_ROOT_CERTS, so cargo-binstall trusts the CA
# - NO_PROXY/no_proxy, so a proxy from the environment is bypassed
# - HTTPS_SERVER_LOG, a file the server appends `METHOD PATH` (and
#   ` Range: ...`, if any) to for every request, so that a test can check
#   what was requested, and how often
#
# The server is stopped when the test script exits.

_https_server_python() {
    if command -v python3 >/dev/null 2>&1; then
        echo python3
    else
        echo python
    fi
}

_https_server_make_certs() {
    local dir="$1"

    # A config file instead of `-subj`, since MSYS mangles `/CN=...` as a path.
    cat >"$dir/ca.cnf" <<'CNF'
[req]
prompt = no
distinguished_name = dn
x509_extensions = ext

[dn]
CN = cargo-binstall e2e test CA

[ext]
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
CNF

    cat >"$dir/server.cnf" <<'CNF'
[req]
prompt = no
distinguished_name = dn

[dn]
CN = localhost

[ext]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost, IP:127.0.0.1, IP:::1
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid, issuer
CNF

    # P-256 keys: much faster to make than RSA 4096, and accepted by every
    # platform verifier.
    openssl ecparam -name prime256v1 -genkey -noout -out "$dir/ca.key"
    openssl req -x509 -new -sha256 -days 2 \
        -key "$dir/ca.key" -config "$dir/ca.cnf" -out "$dir/ca.pem"

    openssl ecparam -name prime256v1 -genkey -noout -out "$dir/server.key"
    openssl req -new -sha256 \
        -key "$dir/server.key" -config "$dir/server.cnf" -out "$dir/server.csr"
    openssl x509 -req -sha256 -days 2 -in "$dir/server.csr" \
        -CA "$dir/ca.pem" -CAkey "$dir/ca.key" -CAcreateserial \
        -extfile "$dir/server.cnf" -extensions ext -out "$dir/server.pem"
}

start_https_server() {
    local serve_dir="$1"
    local routes="${2:-}"
    local cert_dir
    cert_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'https-server')"

    _https_server_make_certs "$cert_dir"

    local port_file="$cert_dir/port"
    HTTPS_SERVER_LOG="$cert_dir/requests.log"
    : >"$HTTPS_SERVER_LOG"
    # -I -S: skip the environment, user site and site-packages, which the
    # server doesn't need, to start faster.
    "$(_https_server_python)" -I -S lib/https_server.py \
        "$serve_dir" "$cert_dir/server.pem" "$cert_dir/server.key" "$port_file" \
        "$HTTPS_SERVER_LOG" ${routes:+"$routes"} &
    HTTPS_SERVER_PID=$!
    # shellcheck disable=SC2064
    trap "kill $HTTPS_SERVER_PID 2>/dev/null || true" EXIT

    # The server writes its port as soon as it listens, before importing
    # http.server and ssl, which usually takes well under 100ms. But on a CI
    # runner busy with the other e2e tests, starting Python alone has taken
    # over 10s (macOS), so allow up to a minute; this only bounds a hang.
    local _
    for _ in $(seq 1 600); do
        [ -s "$port_file" ] && break
        if ! kill -0 "$HTTPS_SERVER_PID" 2>/dev/null; then
            echo "HTTPS server exited before it started listening"
            return 1
        fi
        sleep 0.1
    done
    if [ ! -s "$port_file" ]; then
        echo "HTTPS server did not start listening within a minute"
        return 1
    fi

    HTTPS_SERVER_URL="https://localhost:$(cat "$port_file")"
    export HTTPS_SERVER_URL
    export BINSTALL_HTTPS_ROOT_CERTS="$cert_dir/ca.pem"
    export NO_PROXY="localhost,127.0.0.1,::1${NO_PROXY:+,$NO_PROXY}"
    export no_proxy="$NO_PROXY"
}
