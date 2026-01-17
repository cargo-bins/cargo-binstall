set -euxo pipefail

tmpfile="$(mktemp)"

echo "::group::$1" >> "$tmpfile"

cd e2e-tests
set +e
env -u RUSTFLAGS \
    -u CARGO_BUILD_TARGET \
    bash "$1.sh" \
    "$2" ${@:3} >> "$tmpfile"
exit_status="$?"
set -e

echo "::endgroup::" >> "$tmpfile"

cat "$tmpfile"
exit "$exit_status"
