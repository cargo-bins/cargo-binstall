$ProgressPreference = 'SilentlyContinue'
$ErrorActionPreference = "Stop"
$tmpdir = $Env:TEMP
$BINSTALL_VERSION = $Env:BINSTALL_VERSION
if ($BINSTALL_VERSION -and $BINSTALL_VERSION -notlike 'v*') {
    # prefix version with v
    $BINSTALL_VERSION = "v$BINSTALL_VERSION"
}
# Fetch binaries from `[..]/releases/latest/download/[..]` if _no_ version is
# given, otherwise from `[..]/releases/download/VERSION/[..]`. Note the shifted
# location of '/download'.
$base_url = if (-not $BINSTALL_VERSION) {
    "https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-"
} else {
    "https://github.com/cargo-bins/cargo-binstall/releases/download/$BINSTALL_VERSION/cargo-binstall-"
}

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
# create temp with zip extension (or Expand will complain)
Write-Host "Invoke-WebRequest"
$sw = [Diagnostics.Stopwatch]::StartNew()
$zip = New-TemporaryFile | Rename-Item -NewName { $_ -replace 'tmp$', 'zip' } –PassThru
Invoke-WebRequest -OutFile $zip $url
$zip | Expand-Archive -DestinationPath $tmpdir -Force
$sw.Stop()
$sw.Elapsed

Write-Host ""

Write-Host "Start-Process"
$sw = [Diagnostics.Stopwatch]::StartNew()
$ps = Start-Process -PassThru -Wait "$tmpdir\cargo-binstall.exe" "--self-install"
if ($ps.ExitCode -ne 0) {
    Invoke-Expression "$tmpdir\cargo-binstall.exe -y --force cargo-binstall"
}
$sw.Stop()
$sw.Elapsed

Write-Host "Rm Files"
$sw = [Diagnostics.Stopwatch]::StartNew()
$zip | Remove-Item
$sw.Stop()
$sw.Elapsed
Write-Host "Path"
$sw = [Diagnostics.Stopwatch]::StartNew()
$cargo_home = if ($Env:CARGO_HOME -ne $null) { $Env:CARGO_HOME } else { "$HOME\.cargo" }
if ($Env:Path -split ";" -notcontains "$cargo_home\bin") {
    if (($Env:CI -ne $null) -and ($Env:GITHUB_PATH -ne $null)) {
        Add-Content -Path "$Env:GITHUB_PATH" -Value "$cargo_home\bin"
    } else {
	    Write-Host ""
    	Write-Host "Your path is missing $cargo_home\bin, you might want to add it." -ForegroundColor Red
	    Write-Host ""
     }
}
$sw.Stop()
$sw.Elapsed
