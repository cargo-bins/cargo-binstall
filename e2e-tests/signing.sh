#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

echo Generate tls cert

CERT_DIR=$(mktemp -d 2>/dev/null || mktemp -d -t 'cert-dir')
export CERT_DIR

openssl req -newkey rsa:4096 -x509 -sha256 -days 1 -nodes -out "$CERT_DIR/"ca.pem -keyout "$CERT_DIR/"ca.key -subj '//C=UT/CN=ca.localhost'
openssl req -new -newkey rsa:4096 -sha256 -nodes -out "$CERT_DIR/"server.csr -keyout "$CERT_DIR/"server.key -subj '//C=UT/CN=localhost'
openssl x509 -req -in "$CERT_DIR/"server.csr -CA "$CERT_DIR/"ca.pem -CAkey "$CERT_DIR/"ca.key -CAcreateserial -out "$CERT_DIR/"server.pem -days 1 -sha256 -extfile signing/server.ext

python3 signing/server.py &
server_pid=$!
trap 'kill $server_pid' ERR INT TERM

export BINSTALL_HTTPS_ROOT_CERTS="$CERT_DIR/ca.pem"

signing/wait-for-server.sh

"./$1" binstall --force --manifest-path manifests/signing-Cargo.toml --no-confirm signing-test
"./$1" binstall --force --manifest-path manifests/signing-Cargo.toml --no-confirm --only-signed signing-test
"./$1" binstall --force --manifest-path manifests/signing-Cargo.toml --no-confirm --skip-signatures signing-test

# from quick-install
#"./$1" binstall --force --strategies quick-install --no-confirm --only-signed --target x86_64-unknown-linux-musl zellij@0.38.2

kill $server_pid || true
