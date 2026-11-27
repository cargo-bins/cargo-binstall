$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = "Stop"
$tmpdir = $Env:TEMP
$BINSTALL_VERSION = $Env:BINSTALL_VERSION
# Without a `version`, the action installs its own version, as in the shell
# script.
$checkout_version = $null
if (-not $BINSTALL_VERSION -and $Env:BINSTALL_ACTION_PATH) {
    $cargo_toml = Join-Path $Env:BINSTALL_ACTION_PATH "crates\bin\Cargo.toml"
    $match = if (Test-Path $cargo_toml) {
        Select-String -Path $cargo_toml -Pattern '^version = "(.*)"$' | Select-Object -First 1
    }
    if ($match) {
        $checkout_version = $match.Matches[0].Groups[1].Value
    } else {
        Write-Host "::warning::Could not read the version from $cargo_toml, installing the latest release"
    }
}
if ($BINSTALL_VERSION -eq 'latest') {
    # the latest release, not the action's own version
    $BINSTALL_VERSION = $null
} elseif ($BINSTALL_VERSION -and $BINSTALL_VERSION -notlike 'v*') {
    # prefix version with v
    $BINSTALL_VERSION = "v$BINSTALL_VERSION"
}
# Fetch binaries from `[..]/releases/latest/download/[..]` if _no_ version is
# given, otherwise from `[..]/releases/download/VERSION/[..]`. Note the shifted
# location of '/download'.
$base_url = if (-not $BINSTALL_VERSION) {
    "https://github.com/cargo-bins/cargo-binstall/releases/latest/download/"
} else {
    "https://github.com/cargo-bins/cargo-binstall/releases/download/$BINSTALL_VERSION/"
}

$proc_arch = [Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE", [EnvironmentVariableTarget]::Machine)
if ($proc_arch -eq "AMD64") {
    $arch = "x86_64"
} elseif ($proc_arch -eq "ARM64") {
    $arch = "aarch64"
} else {
    throw "Unsupported Architecture: $proc_arch"
}

$asset = "cargo-binstall-$arch-pc-windows-msvc.zip"
$sw = [Diagnostics.Stopwatch]::StartNew()
# create temp with zip extension (or Expand will complain)
$zip = New-TemporaryFile | Rename-Item -NewName { $_ -replace 'tmp$', 'zip' } -PassThru
$downloaded = $false
if ($checkout_version) {
    try {
        Invoke-WebRequest -Uri "https://github.com/cargo-bins/cargo-binstall/releases/download/v$checkout_version/$asset" -OutFile $zip -UseBasicParsing
        $downloaded = $true
        Write-Verbose -Verbose -Message "Installing v$checkout_version, the version of the action's checkout"
    } catch {
        Write-Verbose -Verbose -Message "Could not download v$checkout_version, installing the latest release: $_"
    }
}
if (-not $downloaded) {
    try {
        Invoke-WebRequest -Uri "$base_url$asset" -OutFile $zip -UseBasicParsing
    } catch {
        throw "Failed to download: $_"
    }
}
$zip | Expand-Archive -DestinationPath $tmpdir -Force
$sw.Stop()
Write-Verbose -Verbose -Message "Download: $($sw.Elapsed.Seconds) seconds"


$sw = [Diagnostics.Stopwatch]::StartNew()
$ps = Start-Process -PassThru -Wait "$tmpdir\cargo-binstall.exe" "--self-install"
if ($ps.ExitCode -ne 0) {
    Invoke-Expression "$tmpdir\cargo-binstall.exe -y --force cargo-binstall"
}
$zip | Remove-Item
$sw.Stop()
Write-Verbose -Verbose -Message "Installation: $($sw.Elapsed.Seconds) seconds"

$sw = [Diagnostics.Stopwatch]::StartNew()
$cargo_home = if ($Env:CARGO_HOME) { $Env:CARGO_HOME } else { "$HOME\.cargo" }
$cargo_bin = Join-Path $cargo_home "bin"
if ($Env:Path.ToLower() -split ";" -notcontains $cargo_bin.ToLower()) {
    if ($Env:CI -and $Env:GITHUB_PATH) {
        Add-Content -Path $Env:GITHUB_PATH -Value $cargo_bin
    } else {
        Write-Verbose -Verbose -Message "Your path is missing $cargo_bin, you might want to add it."
    }
}
$sw.Stop()
Write-Verbose -Verbose -Message "Path addition: $($sw.Elapsed.Seconds) seconds"
