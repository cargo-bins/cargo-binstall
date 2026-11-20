#!/bin/bash

set -euxo pipefail

# taplo ships each binary on its own, gzip-compressed and not in a tarball
# (`taplo-linux-x86_64.gz`), except on Windows where it is in a zip, so this
# test doesn't run there.
# `--strategies crate-meta-data` makes a failure to use it fail the test
# instead of falling back to compiling from source.

"$1" binstall --no-confirm --strategies crate-meta-data \
    taplo-cli@0.10.0 --bin taplo \
    --pkg-url "{ repo }/releases/download/{ version }/taplo-{ target-family }-{ target-arch }.gz" \
    --pkg-fmt gz

taplo --version | grep -F '0.10.0'

# Without `--pkg-fmt`, the format is guessed from the `.gz` in `--pkg-url`.
"$1" binstall --no-confirm --force --strategies crate-meta-data \
    taplo-cli@0.10.0 --bin taplo \
    --pkg-url "{ repo }/releases/download/{ version }/taplo-{ target-family }-{ target-arch }.gz"

taplo --version | grep -F '0.10.0'
