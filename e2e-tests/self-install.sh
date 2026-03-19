#!/bin/bash

set -euxo pipefail

"$1" --self-install

cargo binstall -vV
cargo install --list
