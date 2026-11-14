#!/usr/bin/env bash

# Regenerates the download table in README.md, between the
# `<!-- BEGIN generated:download-table ... -->` and
# `<!-- END generated:download-table -->` markers, from the packages that
# release-packages.yml builds.

set -euo pipefail

cd "$(dirname "$0")/../.."

workflow=.github/workflows/release-packages.yml
readme=README.md
base_url=https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-

# Prints `<target> <glibc version or ->` for each released package.
released_packages() {
    # Matrix entries look like `{ o: ubuntu-latest, t: TARGET, g: 2.17 }`,
    # possibly spread over several lines.
    tr '\n' ' ' <"$workflow" |
        grep -oE '\{[^{}]* t: [a-z0-9_-]+[^{}]*\}' |
        while read -r entry; do
            target=$(grep -oE ' t: [a-z0-9_-]+' <<<" $entry" | awk '{ print $2 }')
            glibc=$(grep -oE ' g: [0-9.]+' <<<" $entry" | awk '{ print $2 }' || true)
            echo "$target ${glibc:--}"
        done
    # The lipo job merges the macOS builds into a universal package.
    if grep -q 'name: universal-apple-darwin' "$workflow"; then
        echo "universal-apple-darwin -"
    fi
}

# Prints `<sort key>|<OS>|<Arch>|<URL>` for a package.
row() {
    local target=$1 glibc=$2 os arch os_key arch_key libc_key ext

    case "$target" in
        *-linux-*) os=Linux os_key=1 ext=tgz ;;
        *-apple-darwin) os=Mac os_key=2 ext=zip ;;
        *-windows-*) os=Windows os_key=3 ext=zip ;;
        *) echo "$0: no OS label for $target" >&2; return 1 ;;
    esac

    case "$os/$target" in
        Linux/x86_64-*) arch='x86\_64' arch_key=1 ;;
        Linux/armv7-*) arch=armv7 arch_key=2 ;;
        Linux/aarch64-*) arch=arm64 arch_key=3 ;;
        Linux/riscv64gc-*) arch=riscv64 arch_key=4 ;;
        Mac/x86_64-*) arch=Intel arch_key=1 ;;
        Mac/aarch64-*) arch='Apple Silicon' arch_key=2 ;;
        Mac/universal-*) arch='Universal<br>(both archs)' arch_key=3 ;;
        Windows/x86_64-*) arch=Intel/AMD arch_key=1 ;;
        Windows/aarch64-*) arch='ARM 64' arch_key=2 ;;
        *) echo "$0: no Arch label for $target" >&2; return 1 ;;
    esac

    # List the static musl build first, it runs on any Linux distro.
    libc_key=0
    case "$target" in
        *-linux-musl*) arch+=' (musl)' ;;
        *-linux-gnu*)
            libc_key=1
            if [ "$glibc" = - ]; then
                arch+=' (glibc)'
            else
                arch+=" (glibc $glibc+)"
            fi
            ;;
    esac

    echo "$os_key$arch_key$libc_key|$os|$arch|${base_url}${target}.${ext}"
}

packages=()
while read -r target glibc; do
    packages+=("$(row "$target" "$glibc")") || exit 1
done < <(released_packages)

if [ ${#packages[@]} -eq 0 ]; then
    echo "$0: found no packages in $workflow" >&2
    exit 1
fi

rows=$(printf '%s\n' "${packages[@]}" | sort | cut -d'|' -f2-)

table=$(awk -F'|' '
    function pad(s, w) {
        return s sprintf("%" (w - length(s)) "s", "")
    }
    {
        n++
        for (i = 1; i <= 3; i++) {
            cell[n, i] = $i
            if (length($i) > width[i]) width[i] = length($i)
        }
    }
    END {
        split("OS|Arch|URL", header, "|")
        for (i = 1; i <= 3; i++) {
            if (length(header[i]) > width[i]) width[i] = length(header[i])
            dashes = sprintf("%" width[i] "s", "")
            gsub(/ /, "-", dashes)
            line1 = line1 "| " pad(header[i], width[i]) " "
            line2 = line2 "| " dashes " "
        }
        print line1 "|"
        print line2 "|"
        for (r = 1; r <= n; r++) {
            line = ""
            for (i = 1; i <= 3; i++) line = line "| " pad(cell[r, i], width[i]) " "
            print line "|"
        }
    }
' <<<"$rows")

TABLE=$table awk '
    /^<!-- END generated:download-table -->$/ { skip = 0 }
    !skip { print }
    /^<!-- BEGIN generated:download-table / { print ENVIRON["TABLE"]; skip = 1; found = 1 }
    END { if (!found) { print "missing download-table markers" > "/dev/stderr"; exit 1 } }
' "$readme" >"$readme.tmp"
mv "$readme.tmp" "$readme"
