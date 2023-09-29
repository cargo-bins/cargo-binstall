#!/bin/ash

# shellcheck shell=dash

set -exuo pipefail

TARGET=${1?}

[ "$(detect-targets)" = "$TARGET" ]

apk update
apk add gcompat

ls -lsha /lib

GNU_TARGET=$(echo "$TARGET" | sed 's/musl/gnu/')

[ "$(detect-targets)" = "$(printf '%s\n%s' "$GNU_TARGET" "$TARGET")" ]

echo
