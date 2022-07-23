#!/bin/bash

set -euxo pipefail

apt update
exec apt install -y --no-install-recommends liblzma-dev libzip-dev libzstd-dev
