#!/bin/bash

set -euxo pipefail

# dist (https://github.com/axodotdev/cargo-dist) can host a release on any
# static file server, set with `simple-download-url` in its config. The
# crate's `dist-workspace.toml` and `Cargo.toml.orig` in its `.crate` are read
# for it, and dist's archive is looked for under it. This crate has no
# repository, so nothing else is looked for.

# shellcheck disable=SC1091
source lib/https-server.sh

target=x86_64-unknown-linux-gnu
archive="signing-test-$target.tar.gz"

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'dist-simple-host')"

# The release, with the archive format set by `unix-archive` below.
mkdir -p "$serve_dir/v0.1.0/signing-test-$target"
tar -xf signing/signing-test.tar -C "$serve_dir/v0.1.0/signing-test-$target" signing-test
tar -czf "$serve_dir/v0.1.0/$archive" -C "$serve_dir/v0.1.0" "signing-test-$target"
rm -r "${serve_dir:?}/v0.1.0/signing-test-$target"

start_https_server "$serve_dir"

# The crate, as `cargo package` makes it for a single-package workspace.
crate_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'dist-simple-host-crate')"
mkdir -p "$crate_dir/signing-test-0.1.0/src"
touch "$crate_dir/signing-test-0.1.0/src/main.rs"
cat >"$crate_dir/signing-test-0.1.0/Cargo.toml" <<'TOML'
[package]
name = "signing-test"
version = "0.1.0"
edition = "2021"
TOML
cat - "$crate_dir/signing-test-0.1.0/Cargo.toml" >"$crate_dir/signing-test-0.1.0/Cargo.toml.orig" <<'TOML'
[workspace.metadata.dist]
unix-archive = ".tar.gz"

TOML
cat >"$crate_dir/signing-test-0.1.0/dist-workspace.toml" <<TOML
[workspace]
members = ["cargo:."]

[dist]
ci = "github"
simple-download-url = "$HTTPS_SERVER_URL/{tag}"
TOML
mkdir -p "$serve_dir/crates/signing-test/0.1.0"
tar -czf "$serve_dir/crates/signing-test/0.1.0/download" -C "$crate_dir" signing-test-0.1.0

# A sparse registry with only that crate.
python="$(_https_server_python)"
mkdir -p "$serve_dir/index/si/gn"
printf '{"dl": "%s/crates"}\n' "$HTTPS_SERVER_URL" >"$serve_dir/index/config.json"
"$python" - "$serve_dir/crates/signing-test/0.1.0/download" >"$serve_dir/index/si/gn/signing-test" <<'PY'
import hashlib, json, sys

with open(sys.argv[1], "rb") as f:
    cksum = hashlib.sha256(f.read()).hexdigest()
print(json.dumps({"name": "signing-test", "vers": "0.1.0", "deps": [], "cksum": cksum,
                  "features": {}, "yanked": False}))
PY

root="$(mktemp -d 2>/dev/null || mktemp -d -t 'dist-simple-host-root')"
log="$(mktemp)"
args=(--force --no-confirm --strategies crate-meta-data --targets "$target"
    --root "$root" --log-level debug)
registry=(--index "sparse+$HTTPS_SERVER_URL/index/")

package_requests() {
    cat "$HTTPS_SERVER_LOG"
    grep -c "signing-test-$target" "$HTTPS_SERVER_LOG" || true
}

"$1" binstall "${args[@]}" "${registry[@]}" signing-test 2>&1 | tee "$log"
test -f "$root/bin/signing-test"
grep -F "Found dist config" "$log"
grep -qxF "GET /v0.1.0/$archive" "$HTTPS_SERVER_LOG"
# Only the archive format dist makes for the target, for each of the 2 tags.
requests="$(package_requests)"
if [ "${requests##*$'\n'}" -gt 2 ]; then
    echo "binstall made more requests than the 2 tags of dist's archive need"
    exit 1
fi

# The pkg-url it was found with is recorded, so that reinstalling only asks
# for the archive.
: >"$HTTPS_SERVER_LOG"
"$1" binstall "${args[@]}" "${registry[@]}" signing-test 2>&1 | tee "$log"
test -f "$root/bin/signing-test"
requests="$(package_requests)"
[ "${requests##*$'\n'}" -eq 1 ]
grep -qxF "GET /v0.1.0/$archive" "$HTTPS_SERVER_LOG"

# `dist = false` opts the package out: nothing is looked for.
manifest_dir="$crate_dir/signing-test-0.1.0"
mv "$manifest_dir/Cargo.toml.orig" "$manifest_dir/Cargo.toml"
printf '\n[package.metadata.dist]\ndist = false\n' >>"$manifest_dir/Cargo.toml"
: >"$HTTPS_SERVER_LOG"
if "$1" binstall "${args[@]}" --manifest-path "$manifest_dir" signing-test 2>&1 | tee "$log"; then
    echo "binstall installed a package despite dist = false and no repository"
    exit 1
fi
requests="$(package_requests)"
if [ "${requests##*$'\n'}" -ne 0 ]; then
    echo "binstall looked under simple-download-url despite dist = false"
    exit 1
fi
