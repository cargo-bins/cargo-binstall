#!/bin/bash
set -euxo pipefail

for o in outputs/*; do
  pushd "$o"

  chmod +x cargo-binstall*
  cp ../../LICENSE.txt ../../README.md .

  target=$(basename "$o" | cut -d. -f1)
  if grep -qE '(apple|windows)' <<< "$target"; then
    zip "../cargo-binstall-${target}.zip" *
  else
    tar cv * | gzip -9 > "../cargo-binstall-${target}.tgz"
  fi

  popd
done
