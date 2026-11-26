#!/bin/bash

set -euxo pipefail

test_cargo_binstall_install() {
  # Test that the installed binaries can be run
  cargo binstall --help >/dev/null

  cargo_binstall_version="$(cargo binstall -V)"
  echo "$cargo_binstall_version"

  [ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]
}

# Testing conflicts of `--index` and `--registry`
set +e

"$1" binstall --index 'sparse+https://index.crates.io/' --registry t1 cargo-binstall
exit_code="$?"

set -e

if [ "$exit_code" != 2 ]; then
    echo "Expected exit code 2, but actual exit code $exit_code"
    exit 1
fi

# A local git index, so that the git registry code path is tested without
# cloning github.com/rust-lang/crates.io-index, which is slow and was the
# only source of network flakes in this test. It holds just the
# cargo-binstall 0.12.0 entry copied from crates.io, and its `dl` still points
# to static.crates.io.
INDEX="$(mktemp -d 2>/dev/null || mktemp -d -t 'index')"
cp -R registry-index/. "$INDEX"
(
  cd "$INDEX"
  git init
  git config user.email 'test@example.com'
  git config user.name 'test'
  git add .
  git commit -m "Add index"
)
if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    INDEX="$(cygpath -w "$INDEX")"
fi

cat >"$CARGO_HOME/config.toml" << EOF
[registries]
t1 = { index = 'file://$INDEX' }
t2 = { index = "sparse+https://index.crates.io/" }
t4 = { replace-with = "t2" }

[registry]
default = "t1"
EOF

# Install binaries using default registry in config
"$1" binstall --force -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+file:" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install

# t1 is a local git index, which binstall reads in place: it leaves no clone
# in cargo's registry/index, and sees new commits right away. Removing the
# crate must make the next run fail to find it, and adding it back must make
# it installable again.
[ ! -e "$CARGO_HOME/registry/index" ]

git -C "$INDEX" rm -q ca/rg/cargo-binstall
git -C "$INDEX" -c user.email='test@example.com' -c user.name='test' commit -q -m "Remove crate"

set +e
"$1" binstall --force -y cargo-binstall@0.12.0
exit_code="$?"
set -e

if [ "$exit_code" = 0 ]; then
    echo "Expected the crate removed from the git index to not be found"
    exit 1
fi

git -C "$INDEX" -c user.email='test@example.com' -c user.name='test' revert --no-edit HEAD
"$1" binstall --force -y cargo-binstall@0.12.0

test_cargo_binstall_install

[ ! -e "$CARGO_HOME/registry/index" ]

# Install binaries using registry t2 in config
"$1" binstall --force --registry t2 -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install

# Install binaries using registry t4 in config
"$1" binstall --force --registry t4 -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install

# Install binaries using registry t3 in env
CARGO_REGISTRIES_t3_INDEX='sparse+https://index.crates.io/' "$1" binstall --force --registry t3 -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install

# Install binaries using index directly
"$1" binstall --force --index 'sparse+https://index.crates.io/' -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install
