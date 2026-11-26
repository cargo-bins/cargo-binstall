#!/bin/bash

set -euxo pipefail

# A `pkg-url` into a repository other than the crate's own, on a host that
# isn't a known git hosting service, and that repository has moved. Its
# candidates are checked and downloaded where it redirects to, without a
# redirect for each request, and it is looked up once for all the crates
# installed together.

# shellcheck disable=SC1091
source lib/https-server.sh

unset CARGO_INSTALL_ROOT

serve_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'candidate-repos')"
pkg_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'candidate-repos-pkg')"
for crate in b3sum@1.5.0 cargo-watch@8.4.0; do
    bin="${crate%@*}"
    tag="$bin-${crate#*@}"
    mkdir -p "$pkg_dir/$bin" "$serve_dir/new/repo/releases/download/$tag"
    # `.exe` too, so that `{ binary-ext }` finds it on Windows.
    for file in "$bin" "$bin.exe"; do
        printf '#!/bin/sh\necho %s\n' "$bin" >"$pkg_dir/$bin/$file"
        chmod +x "$pkg_dir/$bin/$file"
    done
    tar -czf "$serve_dir/new/repo/releases/download/$tag/$bin.tgz" -C "$pkg_dir/$bin" .
done

# /old/repo redirects to /new/repo. Only the repository itself does, so a
# candidate asked for where it was isn't found. Only the first few requests
# are routed, more than one lookup takes, so that a second lookup would still
# be seen.
routes="$serve_dir/routes.json"
cat >"$routes" <<'JSON'
{
    "/old/repo": [
        {"redirect": "/new/repo", "status": 301},
        {"redirect": "/new/repo", "status": 301},
        {"redirect": "/new/repo", "status": 301},
        {"redirect": "/new/repo", "status": 301}
    ]
}
JSON

start_https_server "$serve_dir" "$routes"

# One target, so that the number of requests doesn't depend on how many
# targets the host has.
target="$("$1" -vV | sed -n 's/^build-target: //p')"

"$1" binstall --no-confirm --strategies crate-meta-data --targets "$target" \
    --pkg-url "$HTTPS_SERVER_URL/old/repo/releases/download/{ name }-{ version }/{ name }.tgz" \
    --pkg-fmt tgz --bin-dir "{ bin }{ binary-ext }" \
    b3sum@1.5.0 cargo-watch@8.4.0

cat "$HTTPS_SERVER_LOG"

for bin in b3sum cargo-watch; do
    if [ ! -e "$CARGO_HOME/bin/$bin" ] && [ ! -e "$CARGO_HOME/bin/$bin.exe" ]; then
        echo "$bin was not installed"
        exit 1
    fi
done

# The repository is looked up once, for both crates.
lookups="$(grep -c ' /old/repo$' "$HTTPS_SERVER_LOG" || true)"
if [ "$lookups" != 1 ]; then
    echo "Expected /old/repo to be looked up once, got $lookups lookups"
    exit 1
fi

# No candidate is asked for where it was.
if grep ' /old/repo/' "$HTTPS_SERVER_LOG"; then
    echo "A candidate was asked for in the repository it moved from"
    exit 1
fi

# Each package is found with the GET that downloads it.
for tag in b3sum-1.5.0/b3sum cargo-watch-8.4.0/cargo-watch; do
    gets="$(grep -c "^GET /new/repo/releases/download/$tag.tgz$" "$HTTPS_SERVER_LOG" || true)"
    if [ "$gets" != 1 ]; then
        echo "Expected one GET of $tag.tgz, got $gets"
        exit 1
    fi
done
