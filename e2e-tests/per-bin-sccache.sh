#!/bin/bash

set -euxo pipefail

# sccache releases each binary in its own package:
# `sccache-v{ version }-{ target }` and `sccache-dist-v{ version }-{ target }`,
# the latter only for Linux. Its `pkg-url` and `bin-dir` name `sccache` rather
# than `{ bin }`, so they are overridden here the way it would set them.
#
# `sccache-dist` requires feature `dist-server`, so it is optional: where its
# package is missing it is skipped.
# `--strategies crate-meta-data` makes a failure to use the packages fail the
# test instead of falling back to compiling from source.

install() {
    "$1" binstall --no-confirm --force --strategies crate-meta-data \
        --pkg-url '{ repo }/releases/download/v{ version }/{ bin }-v{ version }-{ target }{ archive-suffix }' \
        --bin-dir '{ bin }-v{ version }-{ target }/{ bin }{ binary-ext }' \
        sccache@0.20.1 "${@:2}"
}

if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    ext=.exe
else
    ext=
fi

check_not_installed() {
    if [ -e "$CARGO_HOME/bin/$1$ext" ]; then
        echo "$1 should not be installed"
        exit 1
    fi
}

install "$1"
sccache --version | grep -F '0.20.1'

case "$OSTYPE" in
    linux*)
        test -x "$CARGO_HOME/bin/sccache-dist"
        cargo uninstall sccache
        check_not_installed sccache
        check_not_installed sccache-dist

        # Only the package of sccache-dist is downloaded and installed.
        install "$1" --bin sccache-dist
        test -x "$CARGO_HOME/bin/sccache-dist"
        check_not_installed sccache
        ;;
    *)
        check_not_installed sccache-dist
        ;;
esac
