#!/bin/bash
# Regenerates the OpenPGP fixtures used by src/signing/openpgp.rs: v4 ones
# with GnuPG, v6 ones with Sequoia (`pip install pysequoia`), and checks all
# of them with both.

set -euxo pipefail

cd "$(dirname "$0")"

GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
trap 'rm -rf "$GNUPGHOME"' EXIT

printf 'binstall OpenPGP test message\n' >message

gpg=(gpg --batch --yes --pinentry-mode loopback --passphrase '')

gen() {
    "${gpg[@]}" --quick-gen-key "binstall-test-$1 <$1@example.com>" "${@:2}" never
    "${gpg[@]}" --armor --export "binstall-test-$1" >"$1.asc"
}

fpr() {
    "${gpg[@]}" --list-keys --with-colons "binstall-test-$1" | awk -F: '/^fpr/ { print $10; exit }'
}

sign() {
    "${gpg[@]}" --local-user "binstall-test-$1" --detach-sign "${@:3}" -o "$2" message
}

gen ed25519 ed25519 sign
gen rsa3072 rsa3072 sign
gen nistp256 nistp256 sign
gen nistp384 nistp384 sign
gen nistp521 nistp521 sign
gen brainpool brainpoolP256r1 sign

# A certification-only primary key with a signing subkey.
"${gpg[@]}" --quick-gen-key "binstall-test-subkey <subkey@example.com>" ed25519 cert never
"${gpg[@]}" --quick-add-key "$(fpr subkey)" ed25519 sign never
"${gpg[@]}" --armor --export binstall-test-subkey >subkey.asc

for key in ed25519 rsa3072 nistp256 nistp384 nistp521 brainpool subkey; do
    sign "$key" "$key.sig"
done
sign ed25519 ed25519.sig.asc --armor
sign nistp256 nistp256-sha512.sig --digest-algo SHA512
sign ed25519 ed25519-sha1.sig --digest-algo SHA1
sign ed25519 ed25519-text.sig --textmode
# Signed by two keys, of which the package only trusts one.
"${gpg[@]}" -u binstall-test-rsa3072 -u binstall-test-ed25519 --detach-sign -o two-signers.sig message

for key in ed25519 rsa3072 nistp256 nistp384 nistp521 subkey; do
    "${gpg[@]}" --verify "$key.sig" message
done

"${PYTHON:-python3}" generate_v6.py
