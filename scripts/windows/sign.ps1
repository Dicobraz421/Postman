# Signing (S4-01, owner decision 1): exes before packaging, the MSI after it (CI, job `release`).
# Only when the certificate secrets are set, otherwise a notice: WINDOWS_CERT_PFX_BASE64 is the .pfx in base64,
# WINDOWS_CERT_PASSWORD its password (env PFX_BASE64, PFX_PASSWORD); the timestamp server is the variable
# WINDOWS_TIMESTAMP_URL (env TIMESTAMP_URL, DigiCert by default).
# Usage: pwsh -File scripts\windows\sign.ps1 <file> [<file> ...]
param([Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Files)
$ErrorActionPreference = "Stop"
if (-not $env:PFX_BASE64 -or -not $env:PFX_PASSWORD) {
  "::notice title=Подпись::Секреты WINDOWS_CERT_PFX_BASE64 и WINDOWS_CERT_PASSWORD не заданы — без подписи: $($Files | Split-Path -Leaf)"
  exit 0
}
$signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" |
  Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1
if (-not $signtool) { throw "signtool.exe not found" }
$ts = if ($env:TIMESTAMP_URL) { $env:TIMESTAMP_URL } else { "http://timestamp.digicert.com" }
$Files = $Files | Where-Object { Test-Path -LiteralPath $_ }
$pfx = Join-Path $env:RUNNER_TEMP "postman-sign.pfx"
[IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:PFX_BASE64))
try {
  & $signtool.FullName sign /fd SHA256 /f $pfx /p $env:PFX_PASSWORD /tr $ts /td SHA256 /d Postman @Files
  if ($LASTEXITCODE) { throw "signtool sign failed ($LASTEXITCODE)" }
} finally {
  Remove-Item $pfx -Force -ErrorAction SilentlyContinue
}
# The internal CA chain may be untrusted on the runner: a warning, not an error.
& $signtool.FullName verify /pa @Files
if ($LASTEXITCODE) { "::warning title=Подпись::signtool verify: цепочка сертификата не проверена на раннере" }
exit 0
