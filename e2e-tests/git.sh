#!/bin/bash

set -euxo pipefail

test_cargo_binstall_install() {
  # Test that the installed binaries can be run
  cargo binstall --help >/dev/null

  cargo_binstall_version="$(cargo binstall -V)"
  echo "$cargo_binstall_version"

  [ "$cargo_binstall_version" = "cargo-binstall 0.12.0" ]
}

GIT="$(mktemp -d 2>/dev/null || mktemp -d -t 'git')"
if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    # Convert it to windows path so `--git "file://$GIT"` would work
    # on windows.
    GIT="$(cygpath -w "$GIT")"
fi

git_source() {
  if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    echo "(git+file:///$(cygpath -m "$GIT")$1)"
  else
    echo "(git+file://$GIT$1)"
  fi
}

git init "$GIT"
cp manifests/github-test-Cargo.toml "$GIT/Cargo.toml"
(
  cd "$GIT"
  git config user.email 'test@example.com'
  git config user.name 'test'
  git add Cargo.toml
  git commit -m "Add Cargo.toml"
  # For `--branch`, `--tag` and `--rev` below, once the default branch has
  # moved past this commit.
  git branch release-0.12
  git tag -a -m 'Release 0.12.0' v0.12.0
)
RELEASE_HASH="$(cd "$GIT" && git rev-parse HEAD)"

# Install binaries using `--git`
"$1" binstall --force --git "file://$GIT" --no-confirm cargo-binstall

test_cargo_binstall_install

perl -0pi -e 's/version = "0.12.0"/version = "0.12.0-dev"/' "$GIT/Cargo.toml"
(
  cd "$GIT"
  git add Cargo.toml
  git commit -m "Use development manifest version"
)

# Install a release from a git manifest whose in-tree version differs from the
# requested release version.
"$1" binstall --force --git "file://$GIT" --version 0.12.0 --no-confirm cargo-binstall

test_cargo_binstall_install

cp -r manifests/workspace/* "$GIT"
(
  cd "$GIT"
  git add .
  git commit -m 'Update to workspace'
)
COMMIT_HASH="$(cd "$GIT" && git rev-parse HEAD)"

source="$(git_source "#$COMMIT_HASH")"

# Install cargo-binstall using `--git`
"$1" binstall --force --git "file://$GIT" --no-confirm cargo-binstall

test_cargo_binstall_install

cat "$CARGO_HOME/.crates.toml"
grep -F "cargo-binstall 0.12.0 $source" <"$CARGO_HOME/.crates.toml"

# Install cargo-watch using `--git`
"$1" binstall --force --git "file://$GIT" --no-confirm cargo-watch

cargo_watch_version="$(cargo watch -V)"
echo "$cargo_watch_version"

[ "$cargo_watch_version" = "cargo-watch 8.4.0" ]

cat "$CARGO_HOME/.crates.toml"
grep -F "cargo-watch 8.4.0 $source" <"$CARGO_HOME/.crates.toml"

# `--branch`, `--tag` and `--rev` check out an older commit than the default
# branch, whose manifest is not a workspace and has version 0.12.0. The source
# recorded in .crates.toml has the reference, like cargo's.
for reference in \
  "--branch release-0.12:?branch=release-0.12" \
  "--tag v0.12.0:?tag=v0.12.0" \
  "--rev $RELEASE_HASH:?rev=$RELEASE_HASH" \
  "--rev ${RELEASE_HASH:0:10}:?rev=${RELEASE_HASH:0:10}"
do
  # Unquoted to split `--branch release-0.12` into two arguments.
  # shellcheck disable=SC2086
  "$1" binstall --force --git "file://$GIT" ${reference%%:*} --no-confirm cargo-binstall

  test_cargo_binstall_install

  cat "$CARGO_HOME/.crates.toml"
  grep -F "cargo-binstall 0.12.0 $(git_source "${reference#*:}#$RELEASE_HASH")" <"$CARGO_HOME/.crates.toml"
done

# Building from source builds what `--git` points to, not the crate with the
# same name in the registry.
output="$("$1" binstall --dry-run --strategies compile --git "file://$GIT" --tag v0.12.0 --no-confirm cargo-binstall 2>&1)"
echo "$output"
grep -F 'cargo install cargo-binstall --git file://' <<<"$output"
grep -F -- '--tag v0.12.0' <<<"$output"

# They need `--git` and conflict with each other.
if "$1" binstall --tag v0.12.0 --no-confirm cargo-binstall; then
  echo "--tag without --git should fail"
  exit 1
fi
if "$1" binstall --git "file://$GIT" --branch release-0.12 --tag v0.12.0 --no-confirm cargo-binstall; then
  echo "--branch with --tag should fail"
  exit 1
fi

# A missing branch fails instead of falling back to the default branch.
if "$1" binstall --force --git "file://$GIT" --branch nope --no-confirm cargo-binstall; then
  echo "a missing --branch should fail"
  exit 1
fi
