# Индекс решений владельца docs/decisions/README.md (R-03, скилл pets-dev): таблица из файлов
# docs/decisions/<YYYY-MM-DD>-<slug>.md, новые сверху; название — первая строка «# …» файла.
# Индекс обновляет полоса docs-gen (раз в день, от main); PR с новым решением индекс не трогает — нет общего файла.
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\decisions-index.ps1          # записать
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\decisions-index.ps1 -Check   # устарел — код 1
# Вывод — UTF-8 без BOM, строки LF. Сам скрипт — UTF-8 с BOM: иначе PowerShell 5.1 читает кириллицу как ANSI.
param([switch]$Check)
$ErrorActionPreference = "Stop"
$dir = [IO.Path]::GetFullPath("$PSScriptRoot\..\docs\decisions")
$index = Join-Path $dir "README.md"
$utf8 = New-Object Text.UTF8Encoding $false

$rows = Get-ChildItem -LiteralPath $dir -Filter "*.md" |
  Where-Object { $_.Name -ne "README.md" } |
  Sort-Object Name -Descending |
  ForEach-Object {
    if ($_.Name -notmatch '^(\d{4}-\d{2}-\d{2})-[a-z0-9-]+\.md$') { throw "$($_.Name): имя не <YYYY-MM-DD>-<slug>.md" }
    $date = $Matches[1]
    $title = [IO.File]::ReadAllLines($_.FullName, $utf8) | Where-Object { $_ -match '^# ' } | Select-Object -First 1
    if (-not $title) { throw "$($_.Name): нет заголовка «# …»" }
    "| $date | [$($title.Substring(2).Trim())]($($_.Name)) |"
  }

$lines = @(
  "# Решения владельца",
  "",
  "> Руками не править: индекс генерирует ``scripts/decisions-index.ps1`` (полоса docs-gen, раз в день).",
  "> Новое решение — свой файл ``docs/decisions/<YYYY-MM-DD>-<slug>.md`` с заголовком «# …»; индекс в том же PR не трогать.",
  "> Решения до 2026-10-24 — ``docs/specs/S0-decisions.md`` (заморожен, в силе).",
  "",
  "| Дата | Решение |",
  "|---|---|"
) + $rows
$doc = ($lines -join "`n") + "`n"

if ($Check) {
  $old = if (Test-Path -LiteralPath $index) { [IO.File]::ReadAllText($index, $utf8).Replace("`r`n", "`n") } else { "" }
  if ($old -ne $doc) { "docs/decisions/README.md устарел: scripts/decisions-index.ps1"; exit 1 }
  "docs/decisions/README.md актуален"
  exit 0
}
[IO.File]::WriteAllText($index, $doc, $utf8)
"decisions-index: $index"
