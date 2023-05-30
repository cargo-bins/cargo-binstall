$ErrorActionPreference = "Stop"
Set-PSDebug -Trace 1
$tmpdir = $Env:TEMP
$base_url = "https://github.com/cargo-bins/cargo-binstall/releases/latest/download/cargo-binstall-"
$type = (Get-ComputerInfo).CsSystemType.ToLower()
if ($type.StartsWith("x64")) {
	$arch = "x86_64"
} elseif ($type.StartsWith("arm64")) {
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
if ($Env:Path -split ";" -notcontains "$HOME\.cargo\bin") {
	Write-Host ""
	Write-Host "Your path is missing $HOME\.cargo\bin, you might want to add it." -ForegroundColor Red
	Write-Host ""
}
