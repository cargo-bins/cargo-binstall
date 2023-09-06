#!/usr/bin/env bash

set -euxo pipefail

cargo binstall -y rsign2

set +x
expect <<EXP
spawn rsign generate -f -p minisign.pub -s minisign.key
expect "Password:"
send -- "$SIGNING_KEY_SECRET\r"
expect "Password (one more time):"
send -- "$SIGNING_KEY_SECRET\r"
expect eof
EXP
set -x

cat >> crates/bin/Cargo.toml <<EOF
[package.metadata.binstall.signing]
algorithm = "minisign"
pubkey = "$(tail -n1 minisign.pub)"
EOF
