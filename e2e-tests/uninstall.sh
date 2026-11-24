#!/bin/bash

set -euxo pipefail

if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    ext=.exe
else
    ext=""
fi

# Copy it to bin to test use of env var `CARGO`
cp "$1" "$CARGO_HOME/bin/"

cargo binstall --no-confirm cargo-watch@8.4.0
cargo uninstall cargo-watch

# `--uninstall` does the same without cargo, aliases included.
cargo binstall --no-confirm --disable-strategies compile --aliases cargo-watch:cw cargo-watch@8.4.0

cargo binstall --uninstall --dry-run cargo-watch
test -e "$CARGO_HOME/bin/cargo-watch$ext"

cargo binstall --uninstall cargo-watch@8.4.0
test ! -e "$CARGO_HOME/bin/cargo-watch$ext"
test ! -e "$CARGO_HOME/bin/cw$ext"
cargo binstall --list | grep "No installed crates found."
if cargo install --list | grep cargo-watch; then
    echo "cargo still lists cargo-watch after --uninstall"
    exit 1
fi

# A package whose binaries were deleted by hand can still be uninstalled.
cargo binstall --no-confirm --disable-strategies compile cargo-watch@8.4.0
rm "$CARGO_HOME/bin/cargo-watch$ext"
cargo binstall --uninstall cargo-watch
cargo binstall --list | grep "No installed crates found."

# A package that is not installed is an error.
if cargo binstall --uninstall cargo-watch; then
    echo "Expected uninstalling a package that is not installed to fail"
    exit 1
fi

# cargo-binstall can uninstall itself, even on Windows where a running
# executable cannot be deleted.
self_install_dir="$(mktemp -d 2>/dev/null || mktemp -d -t 'self-install')"
cp "$1" "$self_install_dir/"
"$self_install_dir/$(basename "$1")" --self-install

"$CARGO_HOME/bin/cargo-binstall$ext" --uninstall cargo-binstall

# On Windows the executable is deleted once the process exits.
for _ in $(seq 1 50); do
    if [ ! -e "$CARGO_HOME/bin/cargo-binstall$ext" ]; then
        break
    fi
    sleep 0.2
done
test ! -e "$CARGO_HOME/bin/cargo-binstall$ext"

"$1" binstall --list | grep "No installed crates found."
