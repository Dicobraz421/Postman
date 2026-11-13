# PAR-05: удаление снимает профиль AppContainer

lane: x
status: done
deps: []
class: B
files: [crates/worker/src/bin/postman-worker.rs, crates/worker/src/lib.rs, crates/worker/src/sandbox/, tools/pack/src/msi.rs, scripts/windows/msi-check.ps1, scripts/windows/install.ps1]
read: [docs/tasks/PAR-AUDIT.md, docs/adr/0011-platform-contract.md]
checks: [fmt, clippy-windows, nextest-pack, ci-windows]
manual: no

## Цель
После удаления Postman на Windows не остаётся профиля AppContainer `Postman.MailWorker` (аудит №46): на macOS и
Linux следа нет, ADR 0011 требует одинаково.

## Что входит
- `postman-worker --remove-appcontainer`: `DeleteAppContainerProfile`; профиля нет — успех (код 0), отказ ОС — код 1.
  На macOS/Linux — успех без действий.
- MSI: CustomAction `RemoveAppContainer` (тип 82: exe из `worker.exe`, код возврата не важен), immediate от имени
  пользователя (профиль в HKCU), до `InstallInitialize`, условие `REMOVE~="ALL" AND NOT UPGRADINGPRODUCTCODE`.
- `install.ps1 -Uninstall` вызывает то же действие.
- `msi-check.ps1`: профиль создаётся до удаления, после `msiexec /x` его нет.

## Приёмка
- CI Windows (`ci.yml`, job `release`): `msi-check.ps1` — «removed: …, AppContainer profile».
- Тест `tools/pack`: строки `CustomAction` и `InstallExecuteSequence`.
