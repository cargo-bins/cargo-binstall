#!/usr/bin/env bash

set -euo pipefail

[[ -z "$AGE_KEY_SECRET" ]] && { echo "!!! Empty age key secret !!!"; exit 1; }
cat >> age.key <<< "$AGE_KEY_SECRET"

set -x

rage --decrypt --identity age.key --output minisign.key minisign.key.age

ts=$(node -e 'console.log((new Date).toISOString())')
git=$(git rev-parse HEAD)
comment="gh=$GITHUB_REPOSITORY git=$git ts=$ts run=$GITHUB_RUN_ID"

for file in "$@"; do
    rsign sign -W -s minisign.key -x "$file.sig" -t "$comment" "$file"
done

rm age.key minisign.key
