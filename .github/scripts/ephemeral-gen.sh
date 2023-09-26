#!/usr/bin/env bash

set -euxo pipefail

cargo binstall -y rsign2
rsign generate -f -W -p minisign.pub -s minisign.key

cat >> crates/bin/Cargo.toml <<EOF
[package.metadata.binstall.signing]
algorithm = "minisign"
pubkey = "$(tail -n1 minisign.pub)"
EOF

set +x
echo "::add-mask::$(tail -n1 minisign.key)"
echo "private=$(tail -n1 minisign.key)" >> "$GITHUB_OUTPUT"
set -x

rm minisign.key
