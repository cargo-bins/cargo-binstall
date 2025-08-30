#!/bin/bash

set -euxo pipefail

unset CARGO_INSTALL_ROOT

#  - `b3sum@<=1.3.3` would test `fetch_crate_cratesio_version_matched` ability
#    to find versions matching <= 1.3.3
#  - `cargo-quickinstall` would test `fetch_crate_cratesio_version_matched` ability
#    to find latest stable version.
#  - `git-mob-tool tests the using of using a binary name (`git-mob`) different
#    from the package name.
crates="b3sum@<=1.3.3 cargo-release@0.24.9 cargo-binstall@0.20.1 cargo-watch@8.4.0 sccache@0.3.3 cargo-quickinstall jj-cli@0.18.0 git-mob-tool@1.6.1"

CARGO_HOME=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')
export CARGO_HOME
othertmpdir=$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-test')
export PATH="$CARGO_HOME/bin:$othertmpdir/bin:$PATH"

mkdir -p "$othertmpdir/bin"

printf 'yes\nyes\n' | "./$1" binstall cargo-quickinstall

echo
ls -lsha $CARGO_HOME
grep "consent_asked = true" $CARGO_HOME/binstall.toml
echo

printf 'yes\n' | "./$1" binstall --force cargo-quickinstall 2>&1 | tee /tmp/out
grep "Opt in to telemetry?" /tmp/out && exit 1 || exit 0