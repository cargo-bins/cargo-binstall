#!/bin/bash

set -euxo pipefail

# A private sparse registry with `auth-required`, whose index and crate
# downloads need `Authorization: secret-token`, and the ways to give
# cargo-binstall that token: `cargo:token` and an external credential
# provider speaking cargo's credential provider protocol.

# shellcheck disable=SC1091
source lib/https-server.sh

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'registry-auth')"

# The crate: just the manifest of signing-test, whose binary is
# signing/signing-test.tar.
crate_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'registry-auth-crate')"
mkdir -p "$crate_dir/signing-test-0.1.0/src"
cp manifests/signing-Cargo.toml "$crate_dir/signing-test-0.1.0/Cargo.toml"
touch "$crate_dir/signing-test-0.1.0/src/main.rs"
mkdir "$serve_dir/dl"
tar -czf "$serve_dir/dl/signing-test-0.1.0.crate" -C "$crate_dir" signing-test-0.1.0

if command -v sha256sum >/dev/null 2>&1; then
    cksum="$(sha256sum "$serve_dir/dl/signing-test-0.1.0.crate" | cut -d ' ' -f 1)"
else
    cksum="$(shasum -a 256 "$serve_dir/dl/signing-test-0.1.0.crate" | cut -d ' ' -f 1)"
fi

cp signing/signing-test.tar signing/signing-test.tar.sig "$serve_dir/"

# Only the registry needs the token, not the release assets.
export HTTPS_SERVER_AUTH="/private/ secret-token"
start_https_server "$serve_dir"

# Two copies of the index: a private one that needs the token, and a public
# one that does not.
for registry in private public; do
    mkdir -p "$serve_dir/$registry/index/si/gn" "$serve_dir/$registry/dl"
    cp "$serve_dir/dl/signing-test-0.1.0.crate" "$serve_dir/$registry/dl/"
    echo "{\"name\":\"signing-test\",\"vers\":\"0.1.0\",\"deps\":[],\"cksum\":\"$cksum\",\"features\":{},\"yanked\":false}" \
        >"$serve_dir/$registry/index/si/gn/signing-test"
done
cat >"$serve_dir/private/index/config.json" <<EOF
{"dl": "$HTTPS_SERVER_URL/private/dl/{crate}-{version}.crate", "auth-required": true}
EOF
cat >"$serve_dir/public/index/config.json" <<EOF
{"dl": "$HTTPS_SERVER_URL/public/dl/{crate}-{version}.crate"}
EOF

install() {
    : >"$HTTPS_SERVER_LOG"
    "$1" binstall --force --no-confirm --registry "$2" \
        --strategies crate-meta-data \
        --pkg-url "$HTTPS_SERVER_URL/signing-test.tar" \
        signing-test@0.1.0
}

write_config() {
    cat >"$CARGO_HOME/config.toml" <<EOF
$1

[registries.private]
index = "sparse+$HTTPS_SERVER_URL/private/index/"

[registries.public]
index = "sparse+$HTTPS_SERVER_URL/public/index/"
EOF
}

log="$serve_dir/output"

# No token: the registry refuses the request, and cargo-binstall says that
# it needs one.
write_config ""
set +e
install "$1" private 2>&1 | tee "$log"
exit_code="${PIPESTATUS[0]}"
set -e
if [ "$exit_code" = 0 ]; then
    echo "Expected the install to fail without a token"
    exit 1
fi
grep -F "registry requires authentication" "$log"
grep -F "GET /private/index/config.json Authorization: no" "$HTTPS_SERVER_LOG"

# `cargo:token`, the default provider, with the token from the environment.
CARGO_REGISTRIES_PRIVATE_TOKEN=secret-token install "$1" private
grep -F "GET /private/index/si/gn/signing-test Authorization: yes" "$HTTPS_SERVER_LOG"
grep -F "GET /private/dl/signing-test-0.1.0.crate Authorization: yes" "$HTTPS_SERVER_LOG"

# The credential provider protocol needs an executable, which a bash script
# is not on Windows.
if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    exit 0
fi

# An external credential provider, see
# https://doc.rust-lang.org/cargo/reference/credential-provider-protocol.html
provider_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'registry-auth-provider')"
cat >"$provider_dir/provider" <<EOF
#!/bin/bash
set -euo pipefail
[ "\$1" = --cargo-plugin ]
echo '{"v":[1]}'
read -r request
printf '%s\n' "\$request" >>"$provider_dir/requests"
echo '{"Ok":{"kind":"get","token":"secret-token","cache":"session","operation_independent":true}}'
EOF
chmod +x "$provider_dir/provider"

# Providers listed later are tried first.
write_config "[registry]
global-credential-providers = [\"cargo:token\", \"$provider_dir/provider --some-arg\"]"

# It is not run for a registry that does not ask for a token.
install "$1" public
[ ! -e "$provider_dir/requests" ]

install "$1" private
grep -F "GET /private/index/si/gn/signing-test Authorization: yes" "$HTTPS_SERVER_LOG"
grep -F "GET /private/dl/signing-test-0.1.0.crate Authorization: yes" "$HTTPS_SERVER_LOG"

# It is run once, with the headers of the 401 response to config.json.
[ "$(wc -l <"$provider_dir/requests")" -eq 1 ]
grep -F '"kind":"get"' "$provider_dir/requests"
grep -F '"operation":"read"' "$provider_dir/requests"
grep -F "\"index-url\":\"sparse+$HTTPS_SERVER_URL/private/index/\"" "$provider_dir/requests"
grep -F '"name":"private"' "$provider_dir/requests"
grep -iF '"www-authenticate: Cargo"' "$provider_dir/requests"
grep -F '"args":["--some-arg"]' "$provider_dir/requests"
