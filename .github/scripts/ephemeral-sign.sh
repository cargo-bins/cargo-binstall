#!/usr/bin/env bash

set -euo pipefail

cat > minisign.key <<< "$SIGNING_KEY"

set -x

cargo binstall -y rsign2

ts=$(date --utc --iso-8601=seconds)
git=$(git rev-parse HEAD)
comment="gh=$GITHUB_REPOSITORY git=$git ts=$ts run=$GITHUB_RUN_ID"

for file in "$@"; do
    rsign sign -W -s minisign.key -x "$file.sig" -t "$comment" "$file"
done

