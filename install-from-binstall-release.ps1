$ErrorActionPreference = "Stop"
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
}
