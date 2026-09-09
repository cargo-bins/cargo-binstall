#!/bin/bash

set -exuo pipefail

TARGET=${1?}

# native targets first; compat targets (e.g. i686 via ia32 compat mode)
# may follow
[ "$(detect-targets | head -n1)" = "$TARGET" ]

apk update
apk add gcompat

ls -lsha /lib

GNU_TARGET=${TARGET//musl/gnu}

[ "$(detect-targets | head -n2)" = "$(printf '%s\n%s' "$GNU_TARGET" "$TARGET")" ]

echo
