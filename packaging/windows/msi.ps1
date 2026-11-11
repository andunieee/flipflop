# Build the release app and package it for Windows, in dist\: the installer (flipflop.wxs, needs WiX 3's candle and
# light in PATH) and the portable folder dist\flipflop (flipflop.exe), which the release zips.
# Usage: powershell -File packaging\windows\msi.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..\..")
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"$' | Select-Object -First 1).Matches[0].Groups[1].Value
cargo build --release --locked -p flipflop-app --bin flipflop-gui
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
$target = (cargo metadata --format-version 1 --no-deps | ConvertFrom-Json).target_directory

New-Item -ItemType Directory -Force -Path dist\flipflop | Out-Null
Copy-Item "$target\release\flipflop-gui.exe" dist\flipflop\flipflop.exe

$obj = Join-Path $env:TEMP "flipflop.wixobj"
candle -nologo -arch x64 "-dVersion=$version" "-dExe=dist\flipflop\flipflop.exe" "-dIcon=packaging\windows\flipflop.ico" -out $obj packaging\windows\flipflop.wxs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
light -nologo -out "dist\flipflop-$version.msi" $obj
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
