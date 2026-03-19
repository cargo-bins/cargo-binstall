#!/usr/bin/env bash

set -euxo pipefail

rsign generate -f -W -p minisign.pub -s minisign.key

set +x
echo "::add-mask::$(tail -n1 minisign.key)"
set -x
