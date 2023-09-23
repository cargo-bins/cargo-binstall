#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

echo Generate tls cert

rm -f ca.pem ca.srl ca.key server.csr server.pem server.key

openssl req -newkey rsa:4096 -x509 -sha256 -days 1 -nodes -out ca.pem -keyout ca.key -subj "/C=UT/CN=ca.localhost"
openssl req -new -newkey rsa:4096 -sha256 -nodes -out server.csr -keyout server.key -subj "/C=UT/CN=localhost"
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -out server.pem -days 1 -sha256 -extfile server.ext

python server.py 2>/dev/null &
server_pid=$!
trap 'kill $server_pid' ERR INT TERM
sleep 10 # for server to come up

export BINSTALL_HTTPS_ROOT_CERTS=$PWD/ca.pem

"./$1" binstall --force --manifest-path "manifests/signing-Cargo.toml" --no-confirm signing-test
"./$1" binstall --force --manifest-path "manifests/signing-Cargo.toml" --no-confirm --only-signed signing-test
"./$1" binstall --force --manifest-path "manifests/signing-Cargo.toml" --no-confirm --skip-signatures signing-test


kill $server_pid || true
