#!/bin/sh

set -eu

# Trace every command in CI, where the output is a log that gets read when an
# install fails, or when asked to with `BINSTALL_DEBUG`. An interactive install
# stays quiet.
if [ -n "${CI:-}" ] || [ -n "${BINSTALL_DEBUG:-}" ]; then
    set -x
fi

user_agent="Mozilla/5.0 (X11; Linux x86_64; rv:60.0) Gecko/20100101 Firefox/81.0"

# `download URL` writes the body of URL to stdout. It uses curl when it is
# installed and falls back to wget otherwise, so that minimal systems (Alpine,
# busybox) that only ship wget can run this script too.
if command -v curl > /dev/null 2>&1; then
    download() {
        curl --retry 10 -A "$user_agent" -L --proto '=https' --tlsv1.2 -sSf "$1"
    }
elif command -v wget > /dev/null 2>&1; then
    if wget --version 2> /dev/null | grep -q 'GNU Wget'; then
        # GNU wget and wget2 retry transient failures on their own. `-q`
        # also hides the error, and `-nv` would print the signed redirect URL
        # on success, so report a failure here instead.
        download() {
            if ! wget --tries=10 -U "$user_agent" --secure-protocol=TLSv1_2 -q -O - "$1"; then
                echo "wget failed to download $1" >&2
                return 1
            fi
        }
    else
        # busybox wget only knows `-q -O -U -T` and has no retry option, so
        # retry here. The body goes to a file first so that a failed attempt
        # never leaves partial output on stdout.
        download() {
            attempt=1
            while ! wget -U "$user_agent" -T 30 -q -O download.tmp "$1" 2> download.err; do
                cat download.err >&2
                if [ "$attempt" -ge 5 ]; then
                    rm -f download.tmp download.err
                    return 1
                fi
                sleep "$attempt"
                attempt=$((attempt + 1))
            done
            # busybox built with its own TLS code instead of `ssl_client` does
            # not check the server certificate, and says so on stderr. Refuse
            # to install a binary that anyone on the path could have swapped.
            if grep -q 'certificate validation not implemented' download.err; then
                cat download.err >&2
                rm -f download.tmp download.err
                echo "This wget does not validate TLS certificates, install curl or a wget that does and retry" >&2
                return 1
            fi
            cat download.tmp
            rm -f download.tmp download.err
        }
    fi
else
    echo "Neither curl nor wget is installed, install one of them and retry" >&2
    exit 1
fi

# Set pipefail if it works in a subshell, disregard if unsupported
# shellcheck disable=SC3040
(set -o pipefail 2> /dev/null) && set -o pipefail

# Without a `version`, the action installs its own version. It passes its path
# because `GITHUB_ACTION_PATH` is that of whichever action runs this script.
checkout_version=
if [ -z "${BINSTALL_VERSION:-}" ] && [ -n "${BINSTALL_ACTION_PATH:-}" ]; then
    cargo_toml="$BINSTALL_ACTION_PATH/crates/bin/Cargo.toml"
    if [ -f "$cargo_toml" ]; then
        checkout_version="$(sed -n '/^version = "\([^"]*\)".*/{s//\1/p;q;}' "$cargo_toml")"
    fi
    if [ -z "$checkout_version" ]; then
        echo "::warning::Could not read the version from $cargo_toml, installing the latest release"
    fi
fi

case "${BINSTALL_VERSION:-}" in
    "") ;; # unset
    latest) BINSTALL_VERSION= ;; # the latest release, not the action's own version
    v*) ;; # already includes the `v`
    *) BINSTALL_VERSION="v$BINSTALL_VERSION" ;; # Add a leading `v`
esac

cd "$(mktemp -d)"

# Fetch binaries from `[..]/releases/latest/download/[..]` if _no_ version is
# given, otherwise from `[..]/releases/download/VERSION/[..]`. Note the shifted
# location of '/download'.
if [ -z "${BINSTALL_VERSION:-}" ]; then
    base_url="https://github.com/cargo-bins/cargo-binstall/releases/latest/download/"
else
    base_url="https://github.com/cargo-bins/cargo-binstall/releases/download/${BINSTALL_VERSION}/"
fi

# `fetch ASSET` downloads ASSET into the current directory, from the checkout's
# version if it has a release, else from the latest. It sets
# `downloaded_version`, empty for the latest.
fetch() {
    if [ -n "$checkout_version" ]; then
        if download "https://github.com/cargo-bins/cargo-binstall/releases/download/v${checkout_version}/$1" > "$1"; then
            downloaded_version="$checkout_version"
            return
        fi
        echo "Could not download v${checkout_version}, installing the latest release" >&2
    fi
    download "${base_url}$1" > "$1"
    downloaded_version="${BINSTALL_VERSION#v}"
}

os="$(uname -s)"
if [ "$os" = "Darwin" ]; then
    fetch cargo-binstall-universal-apple-darwin.zip
    unzip cargo-binstall-universal-apple-darwin.zip
elif [ "$os" = "Linux" ]; then
    machine="$(uname -m)"
    if [ "$machine" = "armv7l" ]; then
        machine="armv7"
    fi
    if [ "$machine" = "riscv64" ]; then
        machine="riscv64gc"
    fi
    target="${machine}-unknown-linux-musl"
    if [ "$machine" = "armv7" ]; then
        target="${target}eabihf"
    fi

    fetch "cargo-binstall-${target}.tgz"
    tar -xvzf "cargo-binstall-${target}.tgz"
elif [ "${OS-}" = "Windows_NT" ]; then
    machine="$(uname -m)"
    target="${machine}-pc-windows-msvc"
    fetch "cargo-binstall-${target}.zip"
    unzip "cargo-binstall-${target}.zip"
else
    echo "Unsupported OS ${os}"
    exit 1
fi

# If `--self-install` fails, install the downloaded version, so a pin holds.
./cargo-binstall --self-install || ./cargo-binstall -y --force "cargo-binstall${downloaded_version:+@$downloaded_version}"

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
