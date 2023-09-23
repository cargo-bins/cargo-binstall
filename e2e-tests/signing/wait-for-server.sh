#!/bin/bash

set -euxo pipefail

CERT="${BINSTALL_HTTPS_ROOT_CERTS?}"

while ! curl --cacert "$CERT" -L https://localhost:4443/signing-test.tar | file -; do
    sleep 10
done
