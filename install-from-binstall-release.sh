#!/bin/bash

set -euxo pipefail

if [ "$OS" = "Windows_NT" ]; then
    # https://github.com/cargo-bins/cargo-binstall/blob/main/install-from-binstall-release.ps1
    powershell -c '$ErrorActionPreference = "Stop"
Set-PSDebug -Trace 1
$tmpdir = $Env:TEMP
$base_url = "https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-"
$proc_arch = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE", [EnvironmentVariableTarget]::Machine)
if ($proc_arch -eq "AMD64") {
	$arch = "x86_64"
} elseif ($proc_arch -eq "ARM64") {
	$arch = "aarch64"
} else {
	Write-Host "Unsupported Architecture: $type" -ForegroundColor Red
	[Environment]::Exit(1)
}
$url = "$base_url$arch-pc-windows-msvc.zip"
Invoke-WebRequest $url -OutFile $tmpdir\cargo-binstall.zip
Expand-Archive -Force $tmpdir\cargo-binstall.zip $tmpdir\cargo-binstall
Write-Host ""
Invoke-Expression "$tmpdir\cargo-binstall\cargo-binstall.exe -y --force cargo-binstall"
Remove-Item -Force $tmpdir\cargo-binstall.zip
Remove-Item -Recurse -Force $tmpdir\cargo-binstall
$cargo_home = if ($Env:CARGO_HOME -ne $null) { $Env:CARGO_HOME } else { "$HOME\.cargo" }
if ($Env:Path -split ";" -notcontains "$cargo_home\bin") {
	Write-Host ""
	Write-Host "Your path is missing $cargo_home\bin, you might want to add it." -ForegroundColor Red
	Write-Host ""
}'
    exit
fi

cd "$(mktemp -d)"

base_url="https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-"

os="$(uname -s)"
if [ "$os" == "Darwin" ]; then
    url="${base_url}universal-apple-darwin.zip"
    curl -LO --proto '=https' --tlsv1.2 -sSf "$url"
    unzip cargo-binstall-universal-apple-darwin.zip
elif [ "$os" == "Linux" ]; then
    machine="$(uname -m)"
    target="${machine}-unknown-linux-musl"
    if [ "$machine" == "armv7" ]; then
        target="${target}eabihf"
    fi

    url="${base_url}${target}.tgz"
    curl -L --proto '=https' --tlsv1.2 -sSf "$url" | tar -xvzf -
else
    echo "Unsupported OS ${os}"
    exit 1
fi

./cargo-binstall -y --force cargo-binstall

CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"

if ! [[ ":$PATH:" == *":$CARGO_HOME/bin:"* ]]; then
    echo
    printf "\033[0;31mYour path is missing %s, you might want to add it.\033[0m\n" "$CARGO_HOME/bin"
    echo
fi
