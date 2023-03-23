#!/bin/bash

set -uxo pipefail

REPO="${REPO?}"
BRANCH="${BRANCH?}"

while true; do
    echo "Fetching list of cache key for $BRANCH"
    cacheKeysForPR="$(gh actions-cache list -R "$REPO" -B "$BRANCH" -L 100 | cut -f 1 )"

    if [ -z "$cacheKeysForPR" ]; then
        break
    fi

    ## Setting this to not fail the workflow while deleting cache keys. 
    set +e
    echo "Deleting caches..."
    for cacheKey in $cacheKeysForPR
    do
        echo Removing "$cacheKey"
        gh actions-cache delete "$cacheKey" -R "$REPO" -B "$BRANCH" --confirm
    done
done
echo "Done cleaning up $BRANCH"
