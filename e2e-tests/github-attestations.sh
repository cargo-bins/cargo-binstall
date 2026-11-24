#!/bin/bash

set -euxo pipefail

root="$(mktemp -d 2>/dev/null || mktemp -d -t 'github-attestations')"
log="$root/log"

install() {
    "$1" binstall --force --no-confirm --root "$root" --strategies crate-meta-data \
        --manifest-path "$2" "${@:3}" cargo-binstall 2>&1 | tee "$log"
    return "${PIPESTATUS[0]}"
}

# cargo-binstall v1.28.0 has both GitHub's immutable release attestation and
# provenance from its release workflow.
install "$1" manifests/github-attestations-Cargo.toml
grep -F "Verified signature for package 'cargo-binstall': GitHub" "$log"

# Without a signing section, --only-signed falls back to GitHub attestations.
install "$1" manifests/github-attestations-unsigned-Cargo.toml --only-signed
grep -F "Verified signature for package 'cargo-binstall': GitHub" "$log"

# Without --only-signed, they are not looked up.
install "$1" manifests/github-attestations-unsigned-Cargo.toml
if grep -F "Verified signature" "$log"; then
    echo "GitHub attestations were checked without --only-signed"
    exit 1
fi

# v1.15.0 predates attestations.
mkdir "$root/v1.15.0"
sed 's/^version = "1.28.0"$/version = "1.15.0"/' manifests/github-attestations-Cargo.toml \
    >"$root/v1.15.0/Cargo.toml"
set +e
install "$1" "$root/v1.15.0/Cargo.toml"
exit_code="$?"
set -e

if [ "$exit_code" != 94 ]; then
    echo "Expected installing v1.15.0 to fail with exit code 94, got $exit_code"
    exit 1
fi
grep -F "cargo-bins/cargo-binstall has no GitHub attestation for this package" "$log"
