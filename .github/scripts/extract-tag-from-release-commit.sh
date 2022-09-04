#!/bin/bash
set -euxo pipefail

message="$(head -n1 <<< "$COMMIT_MESSAGE")"
crate="$(cut -d ' ' -f 2 <<< "${message}")"
version="$(cut -d ' ' -f 3 <<< "${message}")"
echo "::set-output name=crate::${crate}"
echo "::set-output name=version::${version}"
