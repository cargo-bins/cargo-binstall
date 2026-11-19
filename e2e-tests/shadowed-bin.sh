#!/bin/bash

set -euxo pipefail

if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    ext=.exe
else
    ext=""
fi

early="$(mktemp -d 2>/dev/null || mktemp -d -t 'early')"
log="$early/log"
printf '#!/bin/sh\necho shadowing\n' > "$early/cargo-watch$ext"
chmod +x "$early/cargo-watch$ext"

warning="which PATH finds before the one installed to"

# A cargo-watch that PATH finds before the install dir gets a warning.
PATH="$early:$PATH" "$1" binstall --no-confirm --disable-strategies compile cargo-watch@8.4.0 2>&1 | tee "$log"
grep -F "$warning" "$log"

# Without it, there's no warning.
"$1" binstall --no-confirm --force --disable-strategies compile cargo-watch@8.4.0 2>&1 | tee "$log"
if grep -F "$warning" "$log"; then
    echo "Unexpected warning without a shadowing cargo-watch in PATH"
    exit 1
fi

# A file that can't be run doesn't shadow anything. Windows has no execute
# bit: there, the extension decides.
if [ -z "$ext" ]; then
    chmod -x "$early/cargo-watch"
    PATH="$early:$PATH" "$1" binstall --no-confirm --force --disable-strategies compile cargo-watch@8.4.0 2>&1 | tee "$log"
    if grep -F "$warning" "$log"; then
        echo "Unexpected warning for a cargo-watch in PATH without an execute bit"
        exit 1
    fi
fi
