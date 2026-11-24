#!/bin/sh

set -eu

# Trace every command in CI, where the output is a log that gets read when an
# install fails, or when asked to with `BINSTALL_DEBUG`. An interactive install
# stays quiet.
if [ -n "${CI:-}" ] || [ -n "${BINSTALL_DEBUG:-}" ]; then
    set -x
fi

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

# Verify the downloaded archive with `gh attestation verify` against the
# provenance that cargo-binstall's release workflow attests, independently of
# cargo-binstall itself. `BINSTALL_VERIFY_ATTESTATION` is one of:
# - `false` (the default): don't verify;
# - `auto`: verify if `gh` is installed and the release has the attestation,
#   otherwise warn;
# - `true`: verify, and fail if that's not possible.
# Releases before v1.19.0 have no provenance attestation.
verify_attestation() {
    mode="${BINSTALL_VERIFY_ATTESTATION:-false}"
    case "$mode" in
        false) return 0 ;;
        auto | true) ;;
        *)
            echo "BINSTALL_VERIFY_ATTESTATION must be auto, true or false, not '$mode'" >&2
            exit 1
            ;;
    esac

    reason=""
    if ! command -v gh >/dev/null 2>&1; then
        reason="gh (GitHub CLI) is not installed"
    else
        case "${BINSTALL_VERSION:-}" in
            v0.* | v1.[0-9] | v1.[0-9].* | v1.1[0-8] | v1.1[0-8].*)
                reason="$BINSTALL_VERSION predates attestations (v1.19.0)"
                ;;
        esac
    fi

    if [ -n "$reason" ]; then
        if [ "$mode" = true ]; then
            echo "Cannot verify the attestation of $1: $reason" >&2
            exit 1
        fi
        if [ -n "${GITHUB_ACTIONS:-}" ]; then
            echo "::warning::Not verifying the attestation of cargo-binstall: $reason"
        else
            echo "warning: not verifying the attestation of cargo-binstall: $reason" >&2
        fi
        return 0
    fi

    gh attestation verify "$1" \
        --repo cargo-bins/cargo-binstall \
        --signer-workflow cargo-bins/cargo-binstall/.github/workflows/release-cli.yml
}

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
    verify_attestation cargo-binstall-universal-apple-darwin.zip
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

    url="${base_url}${target}.tgz"
    do_curl -O "$url"
    verify_attestation "cargo-binstall-${target}.tgz"
    tar -xvzf "cargo-binstall-${target}.tgz"
elif [ "${OS-}" = "Windows_NT" ]; then
    machine="$(uname -m)"
    target="${machine}-pc-windows-msvc"
    url="${base_url}${target}.zip"
    do_curl -O "$url"
    verify_attestation "cargo-binstall-${target}.zip"
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
