#!/bin/bash

set -euxo pipefail

CERT="${BINSTALL_HTTPS_ROOT_CERTS?}"

while ! curl --cacert "$CERT" --ssl-revoke-best-effort -L https://localhost:4443/signing-test.tar | file -; do
    sleep 10
done
