#!/bin/bash

set -euxo pipefail

CERT="${BINSTALL_HTTPS_ROOT_CERTS?}"

counter=0

while ! curl --cacert "$CERT" --ssl-revoke-best-effort -L https://localhost:4443/signing-test.tar | file -; do
    counter=$(( counter + 1 ))
    if [ "$counter" = "20" ]; then
        echo Failed to connect to https server
        exit 1;
    fi
    sleep 10
done
