#!/usr/bin/env bash

set -euxo pipefail

cat >> crates/bin/Cargo.toml <<EOF
[package.metadata.binstall.signing]
algorithm = "minisign"
pubkey = "$(tail -n1 minisign.pub)"
EOF

cp minisign.pub crates/bin/minisign.pub

