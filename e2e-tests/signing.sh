#!/bin/bash

set -eEuxo pipefail

unset CARGO_INSTALL_ROOT

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

python server.py 2>/dev/null &
server_pid=$!
trap 'kill $server_pid' ERR INT TERM
sleep 5 # for server to come up

export BINSTALL_HTTPS_ROOT_CERTS=$PWD/ca.pem

"./$1" binstall --force --manifest-path "manifests/signing-Cargo.toml" --no-confirm signing-test
"./$1" binstall --force --manifest-path "manifests/signing-Cargo.toml" --no-confirm --only-signed signing-test
"./$1" binstall --force --manifest-path "manifests/signing-Cargo.toml" --no-confirm --skip-signatures signing-test

signing-test >/dev/null

kill $server_pid || true
