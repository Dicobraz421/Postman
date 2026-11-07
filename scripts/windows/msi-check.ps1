# CI check of the MSI (job `release`, the runner is elevated): silent install, the result in the system,
# update to a newer version, refusal to downgrade, removal. Changes the machine - never run it on a workstation.
# The newer package is the same kit under the next patch version (postman-pack msi ... --as).
# Usage: pwsh -File scripts\windows\msi-check.ps1 -Msi <file.msi> -Kit <kit dir> -Pack <postman-pack.exe>
param(
  [Parameter(Mandatory)][string]$Msi,
  [Parameter(Mandatory)][string]$Kit,
  [Parameter(Mandatory)][string]$Pack
)
$ErrorActionPreference = "Stop"
$dir = Join-Path $env:ProgramFiles "Postman"
$lnk = Join-Path ([Environment]::GetFolderPath("CommonPrograms")) "Postman.lnk"
$eventKey = "HKLM:\SYSTEM\CurrentControlSet\Services\EventLog\Application\Postman"
$files = "postman.exe", "postman-worker.exe", "postman-viewer.exe", "README.txt"
$logs = Join-Path $env:RUNNER_TEMP "msi-logs"
New-Item -ItemType Directory -Force $logs | Out-Null

function Msiexec([string]$what, [string]$name, [int[]]$ok = @(0, 3010)) {
  $log = Join-Path $logs "$name.log"
  $p = Start-Process msiexec.exe -ArgumentList "$what /qn /norestart /l*v `"$log`"" -Wait -PassThru
  if ($p.ExitCode -notin $ok) {
    Get-Content $log -Tail 60
    throw "msiexec $what -> $($p.ExitCode), expected $($ok -join '/')"
  }
  "msiexec $name -> $($p.ExitCode)"
}

# Postman entries in "Installed apps" (64-bit view).
function Entries {
  @(Get-ChildItem "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall" |
    Get-ItemProperty | Where-Object { $_.DisplayName -eq "Postman" -and $_.Publisher -eq "Pets" })
}

function Assert-Installed([string]$displayVersion) {
  foreach ($f in $files) { if (-not (Test-Path -LiteralPath (Join-Path $dir $f))) { throw "not installed: $f" } }
  $target = (New-Object -ComObject WScript.Shell).CreateShortcut($lnk).TargetPath
  if ($target -ine (Join-Path $dir "postman.exe")) { throw "Start menu shortcut: '$target'" }
  $msg = (Get-ItemProperty $eventKey).EventMessageFile
  if ($msg -notlike "%SystemRoot%\Microsoft.NET\Framework64\*EventLogMessages.dll") { throw "event source: '$msg'" }
  $e = Entries
  if ($e.Count -ne 1) { throw "expected one Postman entry in Installed apps, found $($e.Count)" }
  if ($e[0].DisplayVersion -ne $displayVersion) { throw "Installed apps version $($e[0].DisplayVersion), expected $displayVersion" }
  if ($e[0].InstallLocation.TrimEnd('\') -ine $dir) { throw "InstallLocation '$($e[0].InstallLocation)'" }
  "installed: $displayVersion, $dir, shortcut, event source"
}

# Product version of a package from its Property table.
function Msi-Version([string]$path) {
  $i = New-Object -ComObject WindowsInstaller.Installer
  $db = $i.OpenDatabase($path, 0)
  $v = $db.OpenView("SELECT Value FROM Property WHERE Property = 'ProductVersion'")
  $v.Execute()
  $r = $v.Fetch()
  $r.StringData(1)
}

$ver = Msi-Version $Msi
Msiexec "/i `"$Msi`"" "install"
Assert-Installed $ver

# Update: the same kit as the next patch version replaces this one (one entry, new version).
$cargo = & $Pack version
$core = ($cargo -split '-')[0] -split '\.'
$next = "$($core[0]).$($core[1]).$([int]$core[2] + 1)"
$newer = Join-Path $env:RUNNER_TEMP "postman-newer.msi"
& $Pack msi $Kit $newer --as $next
if ($LASTEXITCODE) { throw "postman-pack --as $next failed" }
$newVer = Msi-Version $newer
Msiexec "/i `"$newer`"" "update"
Assert-Installed $newVer

# Downgrade is refused (1603) and leaves the newer version in place.
Msiexec "/i `"$Msi`"" "downgrade" @(1603)
Assert-Installed $newVer

Msiexec "/x `"$newer`"" "remove"
if (Test-Path -LiteralPath $dir) { throw "left after removal: $dir" }
if (Test-Path -LiteralPath $lnk) { throw "left after removal: $lnk" }
if (Test-Path $eventKey) { throw "left after removal: $eventKey" }
if ((Entries).Count) { throw "left after removal: Installed apps entry" }
"removed: files, shortcut, event source, Installed apps entry"
