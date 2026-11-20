#!/bin/bash

set -euxo pipefail

crates2="$CARGO_HOME/.crates2.json"
id='cargo-watch 8.4.0 (registry+https://github.com/rust-lang/crates.io-index)'

# Pretend cargo-watch 8.4.0 was built by `cargo install --features x`.
cat >"$CARGO_HOME/.crates.toml" <<TOML
[v1]
"$id" = []
TOML
echo "{\"installs\":{\"$id\":{\"version_req\":null,\"bins\":[],\"features\":[\"x\"],\"all_features\":false,\"no_default_features\":false,\"profile\":\"release\",\"target\":\"x\",\"rustc\":\"x\"}},\"future\":1}" >"$crates2"

"$1" binstall -y --force cargo-watch@8.4.0

# The old build options are replaced by the defaults cargo itself uses for
# packages it only knows from .crates.toml, and unknown keys are kept.
cat "$crates2"
grep -F "\"$id\":{\"all_features\":false,\"bins\":[\"cargo-watch" "$crates2"
grep -F '"features":[],"no_default_features":false,"profile":"release","rustc":null,"target":null,"version_req":null}' "$crates2"
grep -F '"future":1' "$crates2"

# So cargo sees the package as installed instead of rebuilding it.
output="$(cargo install cargo-watch@8.4.0 2>&1)"
echo "$output"
grep -F 'is already installed' <<<"$output"

cargo uninstall cargo-watch
grep -F '"installs":{}' "$crates2"
