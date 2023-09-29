#!/usr/bin/env bash

set -euxo pipefail

rsign generate -f -W -p minisign.pub -s minisign.key

set +x
echo "::add-mask::$(tail -n1 minisign.key)"
set -x

rage --encrypt --recipient "$AGE_KEY_PUBLIC" --output minisign.key.age minisign.key
rm minisign.key
