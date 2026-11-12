# MAC-03: песочница воркера на macOS целиком

lane: sandbox
status: done
deps: [MAC-01]
class: B
files: [crates/worker/src/sandbox/, crates/worker/src/lib.rs, crates/worker/src/bin/, crates/worker/src/wasm.rs, crates/worker/src/ics.rs, crates/worker/tests/, docs/adr/, docs/DEV.md]
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

## Итог (2026-11-12)
- Предел памяти — `sandbox::Limited` (`#[global_allocator]` обоих процессов на macOS), предел от клиента аргументом
  `--memory=…`; сверх — выход `EXIT_MEMORY` = 6, «превышен предел памяти». Хост модулей принимает проверки `Probe`
  (отладочная сборка) — тест его предела.
- Код завершения на macOS — с #571; добавлены имена кода 6 и сигналов (`SIGABRT`, `SIGKILL`, `SIGSEGV`/`SIGBUS`).
- Тесты `tests/macos.rs`: `mach-lookup`, sysctl (с контролем: без песочницы та же проверка удаётся), предел памяти
  воркера и хоста, stderr-файл журнала. Диагностика — `tests/macos_probe.rs`, `POSTMAN_SANDBOX_ALLOW`, `docs/DEV.md`.
- Нестабильный `sandbox::ics_at_the_size_limit` (код 5, сторож): два самых дорогих вида календаря стоили 3–4 с из 8 в
  отладочной сборке, на раннере macOS под нагрузкой — больше 8. Причина — в развёртке: `COUNT`, которого до периода не
  исчерпать, не давал передвинуть начало серии (десятки тысяч дат на серию с 1971 года), а `BYDAY`/`BYSETPOS` у месяца
  и года стоили бюджету в десятки раз меньше, чем развёртке. Исправлено в `ics.rs`; тест с тем же календарём —
  3,4 с вместо ~10 на Windows.
- ADR 0009 — `docs/adr/0009-macos-sandbox.md`, ждёт владельца.
