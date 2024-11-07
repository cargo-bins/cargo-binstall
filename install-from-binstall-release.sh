#!/bin/bash

set -euxo pipefail

BINSTALL_VERSION="${BINSTALL_VERSION:-latest}"

cd "$(mktemp -d)"

base_url="https://github.com/cargo-bins/cargo-binstall/releases/${BINSTALL_VERSION}/download/cargo-binstall-"

os="$(uname -s)"
if [ "$os" == "Darwin" ]; then
    url="${base_url}universal-apple-darwin.zip"
    curl -A "Mozilla/5.0 (X11; Linux x86_64; rv:60.0) Gecko/20100101 Firefox/81.0" -LO --proto '=https' --tlsv1.2 -sSf "$url"
    unzip cargo-binstall-universal-apple-darwin.zip
elif [ "$os" == "Linux" ]; then
    machine="$(uname -m)"
    if [ "$machine" == "armv7l" ]; then
        machine="armv7"
    fi
    target="${machine}-unknown-linux-musl"
    if [ "$machine" == "armv7" ]; then
        target="${target}eabihf"
    fi

    url="${base_url}${target}.tgz"
    curl -A "Mozilla/5.0 (X11; Linux x86_64; rv:60.0) Gecko/20100101 Firefox/81.0" -L --proto '=https' --tlsv1.2 -sSf "$url" | tar -xvzf -
elif [ "${OS-}" = "Windows_NT" ]; then
    machine="$(uname -m)"
    target="${machine}-pc-windows-msvc"
    url="${base_url}${target}.zip"
    curl -A "Mozilla/5.0 (X11; Linux x86_64; rv:60.0) Gecko/20100101 Firefox/81.0" -LO --proto '=https' --tlsv1.2 -sSf "$url"
    unzip "cargo-binstall-${target}.zip"
else
    echo "Unsupported OS ${os}"
    exit 1
fi

./cargo-binstall --self-install

CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

if ! [[ ":$PATH:" == *":$CARGO_HOME/bin:"* ]]; then
    if [ -n "${CI:-}" ] && [ -n "${GITHUB_PATH:-}" ]; then
        echo "$CARGO_HOME/bin" >> "$GITHUB_PATH"
    else
        echo
        printf "\033[0;31mYour path is missing %s, you might want to add it.\033[0m\n" "$CARGO_HOME/bin"
        echo
    fi
fi
