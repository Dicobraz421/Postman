# SBX-02: Windows-песочница без понижения изоляции (аудит PAR-AUDIT)

lane: sandbox
status: done
deps: [LIN-04b]
class: B
files: [crates/worker/src/sandbox/, crates/worker/src/lib.rs, crates/worker/tests/sandbox.rs, docs/00-ARCHITECTURE.md]
read: [docs/tasks/PAR-AUDIT.md]
checks: [clippy, nextest, ci-windows, ci-macos, ci-linux]
manual: no

## Цель
Пункты аудита PAR-AUDIT по полосе sandbox: изоляция не понижается чужими данными, Windows не слабее Unix.

## Итог (2026-11-12)
- №1: повтора в `relaxed` нет — падение или зависание на первом запросе не понижает изоляцию; `relaxed` убран из
  клиента и `spawn` всех ОС.
- №2, №9: без AppContainer воркер не запускается (владелец 2026-11-12: «отказ, только текст»), ошибка «песочница: …» с
  причиной. «Частичная» — только отладочная диагностика `POSTMAN_SANDBOX_DROP=appcontainer`.
- №8: `lockdown` на Windows проверяет `GetProcessMitigationPolicy` (win32k, динамический код); не действует — отказ.
- №14: воркеру не передаются `USERPROFILE`, `APPDATA`, `USERNAME`; остались системные пути и TEMP/TMP/LOCALAPPDATA
  (их подменяет AppContainer).
- №15: `SetErrorMode` без окон ошибок, `WerSetFlags(NOHEAP | NO_UI)` — куча с чужими данными не идёт в отчёт WER;
  `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION` уже был.
- №10: Unix — дескрипторы родителя ≥ 3 закрываются при exec (Linux `close_range(CLOEXEC)`, macOS `fcntl`).
- Тесты `sandbox.rs`: `failure_on_first_request_does_not_lower_isolation`, `worker_gets_no_profile_paths` (все ОС).
  Linux nextest 154/154; Windows и macOS — clippy кросс-целью чистый, тесты — в CI.

## Не вошло
- №16 (уровень из первого ответа), №17 (смысл предела памяти), №44, №45 — отдельной волной.
- `WerAddExcludedApplication` (запись в реестр пользователя, след при удалении — как №46) — не делали.
