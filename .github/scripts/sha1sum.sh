#!/bin/bash

if command -v sha1sum &>/dev/null; then
    exec sha1sum
else
    exec shasum
fi
