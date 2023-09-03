#!/bin/ash

set -exuo pipefail

TARGET=${1?}
ALPINE_TARGET=$(echo "$TARGET" | sed 's/unknown/alpine/')

[ "$(detect-targets)" = "$(printf '%s\n%s' "$ALPINE_TARGET" "$TARGET")" ]

apk update
apk add gcompat

GNU_TARGET=$(echo "$TARGET" | sed 's/musl/gnu/')

[ "$(detect-targets)" = "$(printf '%s\n%s\n%s' "$GNU_TARGET" "$ALPINE_TARGET" "$TARGET")" ]

echo
