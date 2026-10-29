# Postman: install for the current user, no administrator rights (S4-01). Lies next to postman.exe in the zip.
#
#   powershell -ExecutionPolicy Bypass -File .\install.ps1                     install or update
#   powershell -ExecutionPolicy Bypass -File .\install.ps1 -Uninstall          remove files and the Start menu shortcut
#   -Destination <dir>   install somewhere else (default %LOCALAPPDATA%\Programs\Postman)
#   -NoShortcut          no Start menu shortcut
#   -RegisterEventSource (elevated, optional) register the "Postman" source of the Windows Application log;
#                        with -Uninstall - remove it. Does not copy files: an elevated shell may be another user.
#
# The sandboxed worker and viewer host run in an AppContainer, which reads their exes only with a grant for
# ALL APPLICATION PACKAGES (S-1-15-2-1), as in Program Files; the script gives it to the install directory.
# User data (%LOCALAPPDATA%\Postman) and the saved sign-in (Credential Manager) are never touched.
# Messages are English, like the rest of the console output.
param(
  [string]$Destination = (Join-Path $env:LOCALAPPDATA "Programs\Postman"),
  [switch]$Uninstall,
  [switch]$NoShortcut,
  [switch]$RegisterEventSource
)
$ErrorActionPreference = "Stop"

# postman-viewer.exe (viewer host, ADR 0007) is optional: copied and removed when present.
$Files = "postman.exe", "postman-worker.exe", "postman-viewer.exe", "README.txt", "install.ps1"
$Shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "Postman.lnk"
$EventKey = "HKLM:\SYSTEM\CurrentControlSet\Services\EventLog\Application\Postman"
# The .NET message file holds "%1" for every event id: the log shows our text as is (pets-shell writes id 1000).
$EventMessageFile = "%SystemRoot%\Microsoft.NET\Framework64\v4.0.30319\EventLogMessages.dll"

function Test-Admin {
  $id = [Security.Principal.WindowsIdentity]::GetCurrent()
  ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Full([string]$path) {
  [IO.Path]::GetFullPath($path).TrimEnd('\')
}

# Postman started from this directory locks its files.
function Assert-NotRunning([string]$dir) {
  $running = Get-Process -Name "postman", "postman-worker", "postman-viewer" -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and (Full (Split-Path -Parent $_.Path)) -ieq $dir }
  if ($running) { throw "Postman is running from $dir - close it and run the script again" }
}

# Start menu shortcut that points into $dir (another installation's shortcut is left alone).
function Get-OurShortcut([string]$dir) {
  if (-not (Test-Path -LiteralPath $Shortcut)) { return $null }
  $target = (New-Object -ComObject WScript.Shell).CreateShortcut($Shortcut).TargetPath
  if ($target -and (Full (Split-Path -Parent $target)) -ieq $dir) { $Shortcut } else { $null }
}

if ($RegisterEventSource) {
  if (-not (Test-Admin)) { throw "-RegisterEventSource needs an elevated PowerShell (Run as administrator)" }
  if ($Uninstall) {
    if (Test-Path $EventKey) { Remove-Item $EventKey -Recurse -Force; "event source: removed" } else { "event source: not registered" }
    return
  }
  $dll = [Environment]::ExpandEnvironmentVariables($EventMessageFile)
  if (-not (Test-Path -LiteralPath $dll)) { throw "not found: $dll (.NET Framework 4)" }
  New-Item -Path $EventKey -Force | Out-Null
  New-ItemProperty -Path $EventKey -Name EventMessageFile -PropertyType ExpandString -Value $EventMessageFile -Force | Out-Null
  "event source: registered ($EventKey)"
  return
}

$dest = Full $Destination

if ($Uninstall) {
  Assert-NotRunning $dest
  $link = Get-OurShortcut $dest
  if ($link) { Remove-Item -LiteralPath $link -Force; "shortcut: removed" }
  foreach ($f in $Files) {
    $p = Join-Path $dest $f
    if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Force }
  }
  if ((Test-Path -LiteralPath $dest) -and -not (Get-ChildItem -LiteralPath $dest -Force)) { Remove-Item -LiteralPath $dest -Force }
  "removed: $dest"
  "kept: user data $(Join-Path $env:LOCALAPPDATA 'Postman') and the saved sign-in (Credential Manager, 'Postman:...')"
  if (Test-Path $EventKey) { "kept: event source; to remove it run elevated: install.ps1 -Uninstall -RegisterEventSource" }
  return
}

$src = Full $PSScriptRoot
foreach ($f in "postman.exe", "postman-worker.exe") {
  if (-not (Test-Path -LiteralPath (Join-Path $src $f))) { throw "$f is not next to install.ps1 ($src)" }
}
Assert-NotRunning $dest
New-Item -ItemType Directory -Force -Path $dest | Out-Null
if ($src -ine $dest) {
  foreach ($f in $Files) {
    $p = Join-Path $src $f
    if (Test-Path -LiteralPath $p) { Copy-Item -LiteralPath $p -Destination $dest -Force }
  }
}
# Files from a downloaded zip carry the "from the Internet" mark; the user has chosen to install them.
Get-ChildItem -LiteralPath $dest -File | Unblock-File

# Read and execute for AppContainers, inherited by the files (without it the worker sandbox does not start).
& icacls.exe $dest /grant "*S-1-15-2-1:(OI)(CI)(RX)" /T /Q | Out-Null
if ($LASTEXITCODE -ne 0) { throw "icacls failed ($LASTEXITCODE): could not grant ALL APPLICATION PACKAGES access to $dest" }

$exe = Join-Path $dest "postman.exe"
if (-not $NoShortcut) {
  $lnk = (New-Object -ComObject WScript.Shell).CreateShortcut($Shortcut)
  $lnk.TargetPath = $exe
  $lnk.WorkingDirectory = $dest
  $lnk.IconLocation = "$exe,0"
  $lnk.Description = "Postman - mail, calendar and contacts"
  $lnk.Save()
  "shortcut: $Shortcut"
}
$version = (Get-Item -LiteralPath $exe).VersionInfo.ProductVersion
"installed: Postman $version -> $dest"
