#!/bin/bash

set -euxo pipefail

test_cargo_binstall_install() {
  # Test that the installed binaries can be run
  cargo binstall --help >/dev/null

  cargo_binstall_version="$(cargo binstall -V)"
  echo "$cargo_binstall_version"

  [ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]
}

unset CARGO_INSTALL_ROOT

CARGO_HOME="$(mktemp -d 2>/dev/null || mktemp -d -t 'cargo-home')"
export CARGO_HOME
export PATH="$CARGO_HOME/bin:$PATH"

# Testing conflicts of `--index` and `--registry`
set +e

"./$1" binstall --index 'sparse+https://index.crates.io/' --registry t1 cargo-binstall
exit_code="$?"

set -e

if [ "$exit_code" != 2 ]; then
    echo "Expected exit code 2, but actual exit code $exit_code"
    exit 1
fi

cat >"$CARGO_HOME/config.toml" << EOF
[registries]
t1 = { index = "https://github.com/rust-lang/crates.io-index" }
t2 = { index = "sparse+https://index.crates.io/" }
t4 = { replace-with = "t2" }

[registry]
default = "t1"
EOF

# Install binaries using default registry in config
"./$1" binstall --force -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install

# Install binaries using registry t2 in config
"./$1" binstall --force --registry t2 -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install

# Install binaries using registry t4 in config
"./$1" binstall --force --registry t4 -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install

# Install binaries using registry t3 in env
CARGO_REGISTRIES_t3_INDEX='sparse+https://index.crates.io/' "./$1" binstall --force --registry t3 -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install

# Install binaries using index directly
"./$1" binstall --force --index 'sparse+https://index.crates.io/' -y cargo-binstall@0.12.0

grep -F "cargo-binstall 0.12.0 (registry+https://github.com/rust-lang/crates.io-index)" <"$CARGO_HOME/.crates.toml"

test_cargo_binstall_install
