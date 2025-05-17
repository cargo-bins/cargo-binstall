#!/bin/sh

set -eux

do_curl() {
    curl --retry 10 -A "Mozilla/5.0 (X11; Linux x86_64; rv:60.0) Gecko/20100101 Firefox/81.0" -L --proto '=https' --tlsv1.2 -sSf "$@"
}

# Set pipefail if it works in a subshell, disregard if unsupported
# shellcheck disable=SC3040
(set -o pipefail 2> /dev/null) && set -o pipefail

case "${BINSTALL_VERSION:-}" in
    "") ;; # unset
    v*) ;; # already includes the `v`
    *) BINSTALL_VERSION="v$BINSTALL_VERSION" ;; # Add a leading `v`
esac

cd "$(mktemp -d)"

# Fetch binaries from `[..]/releases/latest/download/[..]` if _no_ version is
# given, otherwise from `[..]/releases/download/VERSION/[..]`. Note the shifted
# location of '/download'.
if [ -z "${BINSTALL_VERSION:-}" ]; then
    base_url="https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-"
else
    base_url="https://github.com/cargo-bins/cargo-binstall/releases/download/${BINSTALL_VERSION}/cargo-binstall-"
fi

os="$(uname -s)"
if [ "$os" = "Darwin" ]; then
    url="${base_url}universal-apple-darwin.zip"
    do_curl -O "$url"
    unzip cargo-binstall-universal-apple-darwin.zip
elif [ "$os" = "Linux" ]; then
    machine="$(uname -m)"
    if [ "$machine" = "armv7l" ]; then
        machine="armv7"
    fi
    target="${machine}-unknown-linux-musl"
    if [ "$machine" = "armv7" ]; then
        target="${target}eabihf"
    fi

    url="${base_url}${target}.tgz"
    do_curl "$url" | tar -xvzf -
elif [ "${OS-}" = "Windows_NT" ]; then
    machine="$(uname -m)"
    target="${machine}-pc-windows-msvc"
    url="${base_url}${target}.zip"
    do_curl -O "$url"
    unzip "cargo-binstall-${target}.zip"
else
    echo "Unsupported OS ${os}"
    exit 1
fi

./cargo-binstall --self-install || ./cargo-binstall -y --force cargo-binstall

CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

case ":$PATH:" in
    *":$CARGO_HOME/bin:"*) ;; # Cargo home is already in path
    *) needs_cargo_home=1 ;;
esac

if [ -n "${needs_cargo_home:-}" ]; then
    if [ -n "${CI:-}" ] && [ -n "${GITHUB_PATH:-}" ]; then
        echo "$CARGO_HOME/bin" >> "$GITHUB_PATH"
    else
        echo
        printf "\033[0;31mYour path is missing %s, you might want to add it.\033[0m\n" "$CARGO_HOME/bin"
        echo
    fi
fi
