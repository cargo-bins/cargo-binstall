#!/bin/bash

set -euxo pipefail

# dist (https://github.com/axodotdev/cargo-dist) can host a release on any
# static file server, set with `simple-download-url` in its config. The
# crate's `dist-workspace.toml` is read for it, and the dist-manifest.json
# under it names the package and its sha256. This crate has no repository,
# so nothing else is looked for. (A simple-download-url under the crate's
# own GitHub repository is skipped: the default pkg-url find it there.)

# shellcheck disable=SC1091
source lib/https-server.sh

target=x86_64-unknown-linux-gnu
archive="signing-test-$target.tar.gz"

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'dist-simple-host')"
mkdir -p "$serve_dir/v0.1.0/signing-test-$target"
tar -xf signing/signing-test.tar -C "$serve_dir/v0.1.0/signing-test-$target" signing-test
tar -czf "$serve_dir/v0.1.0/$archive" -C "$serve_dir/v0.1.0" "signing-test-$target"
rm -r "${serve_dir:?}/v0.1.0/signing-test-$target"

python="$(_https_server_python)"
# Writes the manifest, with the sha256 inline as dist >= 0.29 does, or in a
# `.sha256` file next to the archive as older dist does.
write_manifest() {
    "$python" - "$serve_dir/v0.1.0" "$archive" "$target" "$1" <<'PY'
import hashlib, json, os, sys

dir, archive, target, checksum = sys.argv[1:]
with open(os.path.join(dir, archive), "rb") as f:
    sha256 = hashlib.sha256(f.read()).hexdigest()
artifact = {
    "name": archive,
    "kind": "executable-zip",
    "target_triples": [target],
}
artifacts = {archive: artifact}
if checksum == "inline":
    artifact["checksums"] = {"sha256": sha256}
else:
    checksum_file = archive + ".sha256"
    artifact["checksum"] = checksum_file
    artifacts[checksum_file] = {"name": checksum_file, "kind": "checksum"}
    with open(os.path.join(dir, checksum_file), "w") as f:
        f.write(f"{sha256}  {archive}\n")
manifest = {
    "dist_version": "0.31.0",
    "releases": [
        {"app_name": "signing-test", "app_version": "0.1.0", "artifacts": list(artifacts)}
    ],
    "artifacts": artifacts,
}
with open(os.path.join(dir, "dist-manifest.json"), "w") as f:
    json.dump(manifest, f)
PY
}
write_manifest inline

start_https_server "$serve_dir"

crate_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'dist-simple-host-crate')"
mkdir "$crate_dir/src"
touch "$crate_dir/src/main.rs"
cat >"$crate_dir/Cargo.toml" <<'TOML'
[package]
name = "signing-test"
version = "0.1.0"
edition = "2021"
TOML
cat >"$crate_dir/dist-workspace.toml" <<TOML
[workspace]
members = ["cargo:."]

[dist]
ci = "github"
hosting = ["simple"]
simple-download-url = "$HTTPS_SERVER_URL/{tag}"
TOML

root="$(mktemp -d 2>/dev/null || mktemp -d -t 'dist-simple-host-root')"
log="$(mktemp)"
args=(--force --no-confirm --strategies crate-meta-data --targets "$target"
    --root "$root" --manifest-path "$crate_dir" --log-level debug)

"$1" binstall "${args[@]}" signing-test 2>&1 | tee "$log"
test -f "$root/bin/signing-test"
grep -F "Found package in dist-manifest.json" "$log"
grep -F "Verified sha256 checksum of package 'signing-test' from dist-manifest.json" "$log"
cat "$HTTPS_SERVER_LOG"
grep -qxF "GET /v0.1.0/dist-manifest.json" "$HTTPS_SERVER_LOG"
grep -qxF "GET /v0.1.0/$archive" "$HTTPS_SERVER_LOG"

# The manifest's url is recorded, so that reinstalling only asks for it.
: >"$HTTPS_SERVER_LOG"
"$1" binstall "${args[@]}" signing-test 2>&1 | tee "$log"
test -f "$root/bin/signing-test"
cat "$HTTPS_SERVER_LOG"
[ "$(grep -c "dist-manifest.json" "$HTTPS_SERVER_LOG")" -eq 1 ]
grep -qxF "GET /v0.1.0/dist-manifest.json" "$HTTPS_SERVER_LOG"

# The checksum may be in a file instead.
write_manifest file
: >"$HTTPS_SERVER_LOG"
"$1" binstall "${args[@]}" signing-test 2>&1 | tee "$log"
test -f "$root/bin/signing-test"
cat "$HTTPS_SERVER_LOG"
grep -F "Reading sha256 checksum file $HTTPS_SERVER_URL/v0.1.0/$archive.sha256" "$log"
grep -F "Verified sha256 checksum of package 'signing-test' from dist-manifest.json" "$log"
grep -qxF "GET /v0.1.0/$archive.sha256" "$HTTPS_SERVER_LOG"

# `dist = false` opts the package out: nothing is looked for.
printf '\n[package.metadata.dist]\ndist = false\n' >>"$crate_dir/Cargo.toml"
: >"$HTTPS_SERVER_LOG"
if "$1" binstall "${args[@]}" signing-test 2>&1 | tee "$log"; then
    echo "binstall installed a package despite dist = false and no repository"
    exit 1
fi
if grep -F "dist-manifest.json" "$HTTPS_SERVER_LOG"; then
    echo "binstall looked for dist-manifest.json despite dist = false"
    exit 1
fi
