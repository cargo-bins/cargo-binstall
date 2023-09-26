#!/usr/bin/env bash

set -euxo pipefail

cargo binstall -y rsign2 rage
rsign generate -f -W -p minisign.pub -s minisign.key

set +x
echo "::add-mask::$(tail -n1 minisign.key)"
set -x

rage --encrypt --recipient "$AGE_KEY_PUBLIC" --output minisign.key.age minisign.key
rm minisign.key
