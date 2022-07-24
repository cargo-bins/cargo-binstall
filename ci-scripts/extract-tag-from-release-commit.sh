#!/bin/bash
set -euxo pipefail

message="$(head -n1 <<< "$COMMIT_MESSAGE")"
version="$(cut -d ' ' -f 2 <<< "${message}")"
echo "::set-output name=version::${version}"
