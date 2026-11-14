# Builds kindlerdp for the Kindle and assembles dist\, which mirrors the Kindle's USB root.
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$target = 'armv7-unknown-linux-musleabihf'
$gnu = "$env:USERPROFILE\.rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained"
$env:Path = "$env:USERPROFILE\.cargo\bin;$gnu;$env:Path"
if (-not (Get-Command zig -ErrorAction SilentlyContinue)) {
    # winget puts zig on PATH only for shells started after the install
    $zig = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\zig.zig_*\zig-*\zig.exe" | Select-Object -First 1
    if (-not $zig) { throw 'zig not found. Install it: winget install zig.zig' }
    $env:Path = "$($zig.DirectoryName);$env:Path"
}

cargo test
if ($LASTEXITCODE) { throw 'tests failed' }
cargo build --release --target $target
if ($LASTEXITCODE) { throw 'build failed' }

if (Test-Path dist) { Remove-Item dist -Recurse -Force }
Copy-Item kindle dist -Recurse
Copy-Item "target\$target\release\kindlerdp" dist\kindlerdp\
Write-Host "Done. Copy the contents of $PSScriptRoot\dist to the Kindle's root folder."
