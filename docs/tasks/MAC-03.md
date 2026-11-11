# MAC-03: песочница воркера на macOS целиком

lane: sandbox
status: todo
deps: [MAC-01]
class: B
files: [crates/worker/src/sandbox/macos.rs, crates/worker/src/lib.rs, crates/worker/tests/, docs/adr/]
read: [crates/worker/src/sandbox/macos.rs, crates/worker/src/sandbox/linux.rs, docs/decisions/2026-11-11-macos.md]
checks: [clippy, nextest]
manual: no

## Цель
Песочница воркера и хоста модулей на macOS — не слабее Windows (AppContainer + Job Object) и Linux (Landlock +
seccomp).

## Что входит
- Предел памяти: macOS не соблюдает `RLIMIT_AS` — ограничивающий `GlobalAlloc` в воркере (счётчик фичи `alloc-count`
  уже есть) с пределом `MEMORY_LIMIT` / `wasm::JOB_MEMORY`; превышение — выход с понятным кодом, клиент показывает
  его в `exit_name`.
- Код завершения воркера для текста ошибки (`exit_code` сейчас `None`, как на Linux).
- Тесты полной песочницы на macOS сверх общих: `mach-lookup`, sysctl, запись в унаследованный stderr-файл журнала не
  убивает воркер.
- Диагностика, если `sandbox_init` откажет (по образцу `windows_probe`): какой запрет мешает старту.
- ADR «Песочница на macOS»: Seatbelt `deny default` (с MAC-01), почему не App Sandbox + XPC, что открыто и почему.

## Приёмка
- `crates/worker/tests/sandbox.rs`, `smime.rs`, `viewer.rs` зелёные на macOS с `POSTMAN_REQUIRE_FULL_SANDBOX=1`
  (с MAC-01), плюс новые тесты предела памяти.
- ADR принят владельцем (класс B).

## Зависимости
MAC-01 (профиль `deny default`, `sandbox_init`).
