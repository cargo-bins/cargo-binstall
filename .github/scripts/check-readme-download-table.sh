#!/usr/bin/env bash

# Checks that the download table in README.md matches the packages that
# release-packages.yml builds: every released target needs a row, except the
# glibc Linux ones (the table lists the static musl builds for Linux), and
# every row must point at a released package.

set -euo pipefail

cd "$(dirname "$0")/../.."

workflow=.github/workflows/release-packages.yml
base_url=https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-

released_targets() {
    # Matrix entries look like `{ o: macos-latest, t: x86_64-apple-darwin }`.
    grep -oE ' t: [a-z0-9_-]+' "$workflow" | awk '{ print $2 }'
    # The lipo job merges the macOS builds into a universal package.
    if grep -q 'name: universal-apple-darwin' "$workflow"; then
        echo universal-apple-darwin
    fi
}

expected=$(
    released_targets | sort -u | while read -r target; do
        case "$target" in
            *-linux-gnu*) continue ;;
            *-apple-darwin | *-windows-*) ext=zip ;;
            *) ext=tgz ;;
        esac
        echo "${base_url}${target}.${ext}"
    done | sort
)

actual=$(grep -oE "${base_url//./\\.}[a-z0-9_.-]+" README.md | sort)

if ! diff -u --label "expected (from $workflow)" --label "README.md" \
    <(echo "$expected") <(echo "$actual"); then
    echo "README.md download table is out of date with $workflow" >&2
    exit 1
fi
