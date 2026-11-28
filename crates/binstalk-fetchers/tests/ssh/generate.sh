#!/bin/bash
# Regenerates the sshsig fixtures used by src/signing/ssh.rs with OpenSSH.

set -euxo pipefail

cd "$(dirname "$0")"

key_dir="$(mktemp -d)"
trap 'rm -rf "$key_dir"' EXIT

printf 'binstall sshsig test message\n' >message

gen() {
    ssh-keygen -q -N '' -C "binstall-test-$1" -t "${@:2}" -f "$key_dir/$1"
    cp "$key_dir/$1.pub" "$1.pub"
}

sign() {
    ssh-keygen -Y sign -f "$key_dir/$1" "${@:3}" <message >"$2.sig"
}

gen ed25519 ed25519
gen ecdsa256 ecdsa -b 256
gen ecdsa384 ecdsa -b 384
gen ecdsa521 ecdsa -b 521
gen rsa rsa -b 3072

for key in ed25519 ecdsa256 ecdsa384 ecdsa521 rsa; do
    sign "$key" "$key" -n file
done
# The default message hash is sha512; cover sha256 too.
sign rsa rsa-sha256 -n file -O hashalg=sha256
cp rsa.pub rsa-sha256.pub
# A signature for another purpose must not be accepted.
sign ed25519 ed25519-git-namespace -n git

ssh-keygen -l -f ed25519.pub | cut -d' ' -f2 >ed25519.fingerprint

# Cross-check against OpenSSH itself.
for key in ed25519 ecdsa256 ecdsa384 ecdsa521 rsa; do
    printf 'test %s\n' "$(cat "$key.pub")" >"$key_dir/allowed"
    ssh-keygen -Y verify -f "$key_dir/allowed" -I test -n file -s "$key.sig" <message
done
