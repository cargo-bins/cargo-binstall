#!/bin/bash

set -exuo pipefail

sccache_path=$(command -v sccache)

mkdir -p ~/.local/bin/

for each in cl.exe g++ c++ clang++ gcc cc clang clang-cl ; do
    if command -v $each &> /dev/null; then
        ln -s "$sccache_path" ~/.local/bin/$each
    fi
done
