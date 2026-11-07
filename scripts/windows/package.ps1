# S4-01: the delivery zip postman-<version>-windows-x64.zip: postman.exe, postman-worker.exe, README.txt, install.ps1,
# and postman-viewer.exe (S8-24, ADR 0007). WASM viewer modules (viewers/*) are not packaged.
# The MSI postman-<version>-windows-x64.msi (owner decision 2026-11-07) - the same files without install.ps1, made by
# postman-pack.exe of the same build (tools/pack); signed afterwards by scripts\windows\sign.ps1 in CI.
# Takes the exes of a finished release build (cargo build --release --workspace --features postman-worker/viewer);
# the version is the one built into postman.exe (Cargo). Prints the zip path; in GitHub Actions also sets the
# outputs name, dir (the folder with the packaged files, the build artifact), version, msi (its path) and assets
# (S8-39: the files of a GitHub release - the zip, the MSI and the components under their own names,
# docs/DEV.md "Выпуск").
# Usage: powershell -File scripts\windows\package.ps1 [-TargetDir <cargo target dir>] [-Out <dir, default dist>]
param(
  [string]$TargetDir = "",
  [string]$Out = ""
)
$ErrorActionPreference = "Stop"
$root = [IO.Path]::GetFullPath("$PSScriptRoot\..\..")
# Build directory from cargo (honours CARGO_TARGET_DIR) - never a hard-coded target\ (H-01).
if (-not $TargetDir) {
  $TargetDir = (cargo metadata --format-version 1 --no-deps --manifest-path "$root\Cargo.toml" | ConvertFrom-Json).target_directory
}
if (-not $Out) { $Out = Join-Path $root "dist" }
# Relative paths (CI: CARGO_TARGET_DIR=target) - from the current directory, absolute in the outputs.
$TargetDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($TargetDir)
$Out = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Out)
$release = Join-Path $TargetDir "release"
$exe = Join-Path $release "postman.exe"
if (-not (Test-Path -LiteralPath $exe)) { throw "no $exe - run cargo build --release --workspace --features postman-worker/viewer first" }

$version = (Get-Item -LiteralPath $exe).VersionInfo.ProductVersion
if (-not $version) { throw "postman.exe has no version resource (app-gpui/build.rs)" }
$name = "postman-$version-windows-x64"
$dir = Join-Path $Out $name
$zip = Join-Path $Out "$name.zip"
if (Test-Path -LiteralPath $dir) { Remove-Item -LiteralPath $dir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item -LiteralPath $exe, (Join-Path $release "postman-worker.exe") -Destination $dir
# The viewer host lies next to the worker; only a build with the worker feature `viewer` has it.
$viewer = Join-Path $release "postman-viewer.exe"
if (-not (Test-Path -LiteralPath $viewer)) { throw "no $viewer - build with --features postman-worker/viewer" }
Copy-Item -LiteralPath $viewer -Destination $dir
Copy-Item -LiteralPath "$PSScriptRoot\README.txt", "$PSScriptRoot\install.ps1" -Destination $dir
if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }
Compress-Archive -Path "$dir\*" -DestinationPath $zip -CompressionLevel Optimal
# Release files: the zip and the components a user can download alone ("Component missing", S8-38).
$assets = Join-Path $Out "assets"
if (Test-Path -LiteralPath $assets) { Remove-Item -LiteralPath $assets -Recurse -Force }
New-Item -ItemType Directory -Force -Path $assets | Out-Null
Copy-Item -LiteralPath $zip, (Join-Path $dir "postman-worker.exe"), (Join-Path $dir "postman-viewer.exe") -Destination $assets
$pack = Join-Path $release "postman-pack.exe"
if (-not (Test-Path -LiteralPath $pack)) { throw "no $pack - build the whole workspace (cargo build --release --workspace)" }
$msi = Join-Path $assets "$name.msi"
& $pack msi $dir $msi
if ($LASTEXITCODE) { throw "postman-pack msi failed ($LASTEXITCODE)" }
if ($env:GITHUB_OUTPUT) {
  # UTF-8 without BOM in Windows PowerShell 5.1 and in pwsh alike.
  $lines = "name=$name`ndir=$dir`nassets=$assets`nmsi=$msi`nversion=$version`n"
  [IO.File]::AppendAllText($env:GITHUB_OUTPUT, $lines, (New-Object Text.UTF8Encoding $false))
}
"package: $zip"
"package: $msi"
