$ErrorActionPreference = 'Stop'
Set-Location -LiteralPath $PSScriptRoot
$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargoPath = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path -LiteralPath $cargoPath)) { throw 'Install the stable Rust MSVC toolchain and Visual Studio C++ Build Tools first.' }
& $cargoPath rustc --locked --release -p labeler-desktop --bin yolo-labeler -- -C target-feature=+crt-static
if ($LASTEXITCODE -ne 0) { throw 'Rust build failed.' }
$packageDirectory = Join-Path $PSScriptRoot 'dist\yolo-labeler'
New-Item -ItemType Directory -Path $packageDirectory -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target\release\yolo-labeler.exe') -Destination $packageDirectory -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'docs\RUST-EDITION.md') -Destination (Join-Path $packageDirectory 'README.md') -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'docs\THIRD-PARTY.md') -Destination $packageDirectory -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'LICENSE') -Destination $packageDirectory -Force
$dependencyJson = & $cargoPath metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw 'Could not read dependency metadata.' }
$metadata = $dependencyJson | ConvertFrom-Json
$licenseDirectory = Join-Path $packageDirectory 'licenses'
New-Item -ItemType Directory -Path $licenseDirectory -Force | Out-Null
foreach ($package in $metadata.packages | Where-Object source) {
    $dependencyRoot = Split-Path -Parent $package.manifest_path
    $dependencyLicenses = Get-ChildItem -LiteralPath $dependencyRoot -File | Where-Object Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT)'
    $destinationDirectory = Join-Path $licenseDirectory ($package.name + '-' + $package.version)
    New-Item -ItemType Directory -Path $destinationDirectory -Force | Out-Null
    foreach ($license in $dependencyLicenses) { Copy-Item -LiteralPath $license.FullName -Destination $destinationDirectory -Force }
    $package | Select-Object name,version,license,repository | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $destinationDirectory 'package.json') -Encoding utf8
}
$archive = Join-Path $PSScriptRoot 'dist\yolo-labeler-windows-x64.zip'
$packageFiles = @((Join-Path $packageDirectory 'yolo-labeler.exe'), (Join-Path $packageDirectory 'README.md'), (Join-Path $packageDirectory 'THIRD-PARTY.md'), (Join-Path $packageDirectory 'LICENSE'), $licenseDirectory)
Compress-Archive -LiteralPath $packageFiles -DestinationPath $archive -Force
Write-Output "Distribution: $archive"
