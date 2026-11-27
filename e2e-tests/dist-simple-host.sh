#!/bin/bash

set -euxo pipefail

# dist (https://github.com/axodotdev/cargo-dist) can host a release on any
# static file server, set with `simple-download-url` in its config. The
# crate's `dist-workspace.toml` is read for it, and dist's archive name is
# looked for under it. This crate has no repository, so nothing else is
# looked for. (A simple-download-url under the crate's own GitHub repository
# is skipped: the default pkg-url find the release there.)

# shellcheck disable=SC1091
source lib/https-server.sh

target=x86_64-unknown-linux-gnu
archive="signing-test-$target.tar.gz"

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'dist-simple-host')"
mkdir -p "$serve_dir/v0.1.0/signing-test-$target"
tar -xf signing/signing-test.tar -C "$serve_dir/v0.1.0/signing-test-$target" signing-test
tar -czf "$serve_dir/v0.1.0/$archive" -C "$serve_dir/v0.1.0" "signing-test-$target"
rm -r "${serve_dir:?}/v0.1.0/signing-test-$target"

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
cat "$HTTPS_SERVER_LOG"
grep -qF "/v0.1.0/$archive" "$HTTPS_SERVER_LOG"
# Only dist's archive name and formats are tried, for each tag.
if [ "$(wc -l <"$HTTPS_SERVER_LOG")" -gt 17 ]; then
    echo "binstall made more requests than dist's archive names need"
    exit 1
fi
if grep -F ".txz" "$HTTPS_SERVER_LOG"; then
    echo "binstall tried an extension dist doesn't use"
    exit 1
fi

# The pkg-url it was found with is recorded, so that reinstalling only asks
# for the archive.
: >"$HTTPS_SERVER_LOG"
"$1" binstall "${args[@]}" signing-test 2>&1 | tee "$log"
test -f "$root/bin/signing-test"
cat "$HTTPS_SERVER_LOG"
grep -qxF "GET /v0.1.0/$archive" "$HTTPS_SERVER_LOG"
[ "$(wc -l <"$HTTPS_SERVER_LOG")" -eq 1 ]

# `hosting` without `simple` leaves the simple host out.
sed -i.bak 's/^hosting = \["simple"\]$/hosting = ["github"]/' "$crate_dir/dist-workspace.toml"
: >"$HTTPS_SERVER_LOG"
if "$1" binstall "${args[@]}" signing-test 2>&1 | tee "$log"; then
    echo "binstall installed a package from the simple host despite hosting = [\"github\"]"
    exit 1
fi
if [ -s "$HTTPS_SERVER_LOG" ]; then
    echo "binstall looked under simple-download-url despite hosting = [\"github\"]"
    exit 1
fi
mv "$crate_dir/dist-workspace.toml.bak" "$crate_dir/dist-workspace.toml"

# `dist = false` opts the package out: nothing is looked for.
printf '\n[package.metadata.dist]\ndist = false\n' >>"$crate_dir/Cargo.toml"
: >"$HTTPS_SERVER_LOG"
if "$1" binstall "${args[@]}" signing-test 2>&1 | tee "$log"; then
    echo "binstall installed a package despite dist = false and no repository"
    exit 1
fi
if [ -s "$HTTPS_SERVER_LOG" ]; then
    echo "binstall looked under simple-download-url despite dist = false"
    exit 1
fi
