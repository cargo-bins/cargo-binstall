#!/bin/bash

set -euxo pipefail

# Man pages and shell completions in a package, named after its binaries,
# are installed to $CARGO_HOME/share, next to $CARGO_HOME/bin.

unset CARGO_INSTALL_ROOT

manifest="$CARGO_HOME/binstall/crates-v1.json"
share="$CARGO_HOME/share"
args=(-y --strategies crate-meta-data)
log="$(mktemp)"

if [ "$OSTYPE" = "cygwin" ] || [ "$OSTYPE" = "msys" ]; then
    # Not installed on Windows.
    "$1" binstall "${args[@]}" skim@5.8.0
    [ ! -e "$share" ]
    exit 0
fi

# A file that binstall didn't install is not replaced without --force.
mkdir -p "$share/fish/vendor_completions.d"
echo foreign > "$share/fish/vendor_completions.d/sk.fish"

# skim's archive has man/man1/sk.1 and shell/completion.{bash,zsh,fish}.
"$1" binstall "${args[@]}" skim@5.8.0 2>&1 | tee "$log"
sk --version | grep -F '5.8.0'

grep -F 'And the following man pages and shell completions:' "$log"
grep -F 'Not replacing' "$log" | grep -F "sk.fish"
grep -F '.TH' "$share/man/man1/sk.1"
grep -F 'sk' "$share/bash-completion/completions/sk"
head -n 1 "$share/zsh/site-functions/_sk" | grep -F '#compdef sk'
[ "$(cat "$share/fish/vendor_completions.d/sk.fish")" = foreign ]
# Key bindings and the nushell completion are not completions binstall installs.
[ "$(find "$share" -type f | wc -l)" -eq 5 ]

grep -F '"share_files":["man/man1/sk-tmux.1","man/man1/sk.1","bash-completion/completions/sk","zsh/site-functions/_sk"]' "$manifest"

# On update, the files installed for skim are replaced, and the ones the new
# version no longer has are removed.
touch "$share/man/man1/sk-old.1"
sed -i.bak 's|"share_files":\["man/man1/sk-tmux.1"|"share_files":["man/man1/sk-old.1","man/man1/sk-tmux.1"|' "$manifest"
echo stale > "$share/bash-completion/completions/sk"

"$1" binstall "${args[@]}" skim@5.9.0 2>&1 | tee "$log"
sk --version | grep -F '5.9.0'
[ ! -e "$share/man/man1/sk-old.1" ]
[ "$(cat "$share/bash-completion/completions/sk")" != stale ]
[ "$(cat "$share/fish/vendor_completions.d/sk.fish")" = foreign ]

# --force replaces the file of someone else.
"$1" binstall "${args[@]}" --force skim@5.9.0
grep -F 'complete -c sk' "$share/fish/vendor_completions.d/sk.fish"
grep -F '"fish/vendor_completions.d/sk.fish"' "$manifest"

# Packages not made by dist ship them too, e.g. ripgrep in complete/ and doc/.
"$1" binstall "${args[@]}" ripgrep@14.1.1
head -n 1 "$share/zsh/site-functions/_rg" | grep -F '#compdef rg'
[ -f "$share/man/man1/rg.1" ]
[ -f "$share/bash-completion/completions/rg" ]
[ -f "$share/fish/vendor_completions.d/rg.fish" ]

# --no-share-files installs none.
rm -r "$share"
"$1" binstall "${args[@]}" --force --no-share-files skim@5.9.0
[ ! -e "$share" ]
if [ "$(jq -s 'map(select(.name == "skim") | has("share_files")) | any' "$manifest")" = true ]; then
    echo "share_files recorded with --no-share-files"
    exit 1
fi
