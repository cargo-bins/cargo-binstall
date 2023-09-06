#!/usr/bin/env bash

set -euxo pipefail

cargo binstall -y rsign2

ts=$(date --utc --iso-8601=seconds)
git=$(git rev-parse HEAD)
comment="gh=$GITHUB_REPOSITORY git=$git ts=$ts run=$GITHUB_RUN_ID"

set +x
for file in "$@"; do expect <<EXP
spawn rsign sign -s minisign.key -x "$file.sig" -t "$comment" "$file"
expect "Password:"
send -- "$SIGNING_KEY_SECRET\r"
expect eof
EXP
done

