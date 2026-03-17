#!/usr/bin/env bash

set -euxo pipefail

ts=$(node -e 'console.log((new Date).toISOString())')
git=$(git rev-parse HEAD)
comment="gh=$GITHUB_REPOSITORY git=$git ts=$ts run=$GITHUB_RUN_ID"

for file in "$@"; do
    rsign sign -W -s minisign.key -x "$file.sig" -t "$comment" "$file"
done
