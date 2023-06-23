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

GIT="$(mktemp -d 2>/dev/null || mktemp -d -t 'git')"
if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    # Convert it to windows path so `--git "file://$GIT"` would work
    # on windows.
    GIT="$(cygpath -w "$GIT")"
fi

git init "$GIT"
cp manifests/github-test-Cargo.toml "$GIT/Cargo.toml"
(
  cd "$GIT"
  git config user.email 'test@example.com'
  git config user.name 'test'
  git add Cargo.toml
  git commit -m "Add Cargo.toml"
)

# Install binaries using `--git`
"./$1" binstall --force --git "file://$GIT" --no-confirm cargo-binstall

test_cargo_binstall_install

cp -r manifests/workspace/* "$GIT"
(
  cd "$GIT"
  git add .
  git commit -m 'Update to workspace'
)

# Install binaries using `--git`
"./$1" binstall --force --git "file://$GIT" --no-confirm cargo-binstall

test_cargo_binstall_install

# Install binaries using `--git`
"./$1" binstall --force --git "file://$GIT" --no-confirm cargo-watch

cargo_watch_version="$(cargo watch -V)"
echo "$cargo_watch_version"

[ "$cargo_watch_version" = "cargo-watch 8.4.0" ]
