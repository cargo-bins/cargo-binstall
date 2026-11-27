#!/bin/bash

set -euxo pipefail

# A downloaded binary that can't run on this host (it needs a newer glibc,
# or the musl dynamic loader on a glibc host) is skipped in favour of the
# next detected target, unless the targets were given with --targets.
#
# The fixtures are copies of the host's own `true`, patched like a binary
# built elsewhere would be.

# shellcheck disable=SC1091
source lib/https-server.sh

python="$(_https_server_python)"
true_bin="$(readlink -f "$(type -P true)")"

# Patch the copy of `true` at $1: replace every occurrence of $2 with $3, NUL
# padded to the same length so that no offset moves.
patch_bin() {
    "$python" - "$@" <<'PY'
import sys
path, old, new = sys.argv[1], sys.argv[2].encode(), sys.argv[3].encode()
assert len(new) <= len(old)
data = open(path, "rb").read()
assert old in data, (path, old)
open(path, "wb").write(data.replace(old, new.ljust(len(old), b"\0")))
PY
}

glibc_interp="$(readelf -l "$true_bin" | sed -n 's/.*interpreter: \(.*\)]$/\1/p')"
if [ -z "$glibc_interp" ] || ! grep -q 'GLIBC_2\.' "$true_bin"; then
    echo "$true_bin is not a dynamically linked glibc program, skipping"
    exit 0
fi

serve_dir="$(mktemp -d)"
start_https_server "$serve_dir"

# Make $serve_dir/signing-test-$1.tar, holding a copy of `true` as
# signing-test, patched by the rest of the arguments if any.
make_tar() {
    local target="$1" dir
    shift
    dir="$(mktemp -d)"
    cp "$true_bin" "$dir/signing-test"
    if [ $# -gt 0 ]; then
        patch_bin "$dir/signing-test" "$@"
    fi
    tar -cf "$serve_dir/signing-test-$target.tar" -C "$dir" signing-test
    rm -r "$dir"
}

binstall() {
    "$1" binstall --force --manifest-path manifests/signing-Cargo.toml --no-confirm \
        --strategies crate-meta-data --skip-signatures \
        --pkg-url "$HTTPS_SERVER_URL/signing-test-{ target }.tar" \
        --bin-dir "{ bin }" "${@:2}" signing-test
}

arch="$(uname -m)"
gnu="$arch-unknown-linux-gnu"
musl="$arch-unknown-linux-musl"

# The gnu build needs a glibc from the future, so the musl one is installed.
make_tar "$gnu" GLIBC_2. GLIBC_9.
make_tar "$musl"
root="$(mktemp -d)"
binstall "$1" --root "$root" 2>&1 | tee "$root/log"
grep -q "can't run on this system: it needs glibc 9\." "$root/log"
cmp "$root/bin/signing-test" "$true_bin"

# Given explicitly, the target is trusted: the gnu build is installed as is.
root="$(mktemp -d)"
binstall "$1" --root "$root" --targets "$gnu"
if cmp -s "$root/bin/signing-test" "$true_bin"; then
    echo "--targets $gnu installed the musl build"
    exit 1
fi

# A musl build linked dynamically can't run without the musl loader.
musl_interp="/lib/ld-musl-$arch.so.1"
if [ ! -e "$musl_interp" ]; then
    rm "$serve_dir"/*.tar
    make_tar "$musl" "$glibc_interp" "$musl_interp"
    root="$(mktemp -d)"
    if binstall "$1" --root "$root" 2>&1 | tee "$root/log"; then
        echo "a dynamically linked musl binary was installed on a glibc host"
        exit 1
    fi
    grep -q "dynamically linked against musl libc" "$root/log"
    [ ! -e "$root/bin/signing-test" ]
fi
