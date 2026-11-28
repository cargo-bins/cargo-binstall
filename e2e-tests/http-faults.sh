#!/bin/bash

set -euxo pipefail

# How cargo-binstall copes with a flaky server: error statuses it should
# retry, redirects, and a download cut short. The package is
# signing/signing-test.tar, installed with --skip-signatures since signing
# is tested in signing.sh.

# shellcheck disable=SC1091
source lib/https-server.sh

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'http-faults')"
for name in retry bad-gateway ratelimit down moved-to truncated truncated-no-range silent stalled; do
    cp signing/signing-test.tar "$serve_dir/$name.tar"
done

# The responses to give to the first requests for each path, before it is
# served normally.
routes="$serve_dir/routes.json"
cat >"$routes" <<'JSON'
{
    "/retry.tar": [{"status": 503}, {"status": 503}],
    "/bad-gateway.tar": [{"status": 502}, {"status": 500}],
    "/ratelimit.tar": [{"status": 429, "headers": {"Retry-After": "1"}}],
    "/down.tar": [
        {"status": 503}, {"status": 503}, {"status": 503},
        {"status": 503}, {"status": 503}, {"status": 503}
    ],
    "/moved.tar": [{"redirect": "/moved-to.tar"}, {"redirect": "/moved-to.tar"}],
    "/truncated.tar": [{"truncate": 1000}],
    "/truncated-no-range.tar": [
        {"truncate": 1000},
        {"ignore_range": true}, {"ignore_range": true}, {"ignore_range": true},
        {"ignore_range": true}, {"ignore_range": true}, {"ignore_range": true}
    ],
    "/silent.tar": [{"stall": null}],
    "/stalled.tar": [{"stall": 1000}]
}
JSON

start_https_server "$serve_dir" "$routes"

# One target, so that the number of requests doesn't depend on how many
# targets the host has.
target="$("$1" -vV | sed -n 's/^build-target: //p')"

install() {
    "$1" binstall --force --manifest-path manifests/signing-Cargo.toml --no-confirm \
        --strategies crate-meta-data --skip-signatures --targets "$target" \
        --pkg-url "$HTTPS_SERVER_URL/$2" "${@:3}" signing-test
}

requests_for() {
    grep -cE " /$1( |\$)" "$HTTPS_SERVER_LOG" || true
}

expect_failure() {
    set +e
    install "$@"
    exit_code="$?"
    set -e

    if [ "$exit_code" != 94 ]; then
        echo "Expected installing $2 to fail with exit code 94, got $exit_code"
        exit 1
    fi
}

# 503 twice, then 200: retried, and installed.
install "$1" retry.tar
[ "$(requests_for retry.tar)" -ge 3 ]

# 502, 500, then 200: transient server errors are retried too.
install "$1" bad-gateway.tar
[ "$(requests_for bad-gateway.tar)" -ge 3 ]

# 429 with Retry-After, then 200.
install "$1" ratelimit.tar
[ "$(requests_for ratelimit.tar)" -ge 2 ]

# 503 every time: give up after a bounded number of attempts, don't hang.
expect_failure "$1" down.tar
down_requests="$(requests_for down.tar)"
echo "down.tar was requested $down_requests times"
[ "$down_requests" -ge 2 ] && [ "$down_requests" -le 6 ]

# A redirect is followed.
install "$1" moved.tar
[ "$(requests_for moved-to.tar)" -ge 1 ]

# A download cut short is continued with a Range request. The GET that found
# the package is the download, so that is the only other request.
install "$1" truncated.tar
grep -E "^GET /truncated.tar Range: bytes=[1-9][0-9]*-$" "$HTTPS_SERVER_LOG"
[ "$(requests_for truncated.tar)" -eq 2 ]

# One that can't be continued, since the server answers the Range request
# with the whole file again, fails, and installs nothing.
root="$(mktemp -d 2>/dev/null || mktemp -d -t 'http-faults-root')"
expect_failure "$1" truncated-no-range.tar --root "$root"
if [ -n "$(ls -A "$root/bin" 2>/dev/null)" ]; then
    echo "A truncated download installed something:"
    ls -la "$root/bin"
    exit 1
fi

# A server that accepts the request and never answers it, and one that stops
# sending partway through the body: given up on after `--http-timeout` (or
# `CARGO_HTTP_TIMEOUT`) instead of waiting forever, then retried, and installed.
install "$1" silent.tar --http-timeout 2
[ "$(requests_for silent.tar)" -ge 2 ]

CARGO_HTTP_TIMEOUT=2 install "$1" stalled.tar
grep -E "^GET /stalled.tar Range: bytes=[1-9][0-9]*-$" "$HTTPS_SERVER_LOG"
[ "$(requests_for stalled.tar)" -eq 2 ]

cat "$HTTPS_SERVER_LOG"
