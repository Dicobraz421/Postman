# MAC-01: собирается и запускается на macOS в CI

lane: x
status: done
deps: []
class: B
files: [.github/workflows/macos.yml, scripts/macos/, crates/worker/src/sandbox/, crates/core/src/paths.rs, crates/core/src/viewers.rs, crates/core/src/smime/skzi.rs, crates/backend-ldap/, crates/demo/src/lib.rs, crates/ews/src/auth.rs, app-gpui/src/sys/, app-gpui/src/main.rs, app-gpui/src/diagnostics.rs, app-gpui/src/reading.rs, app-gpui/src/settings_keys.rs, app-gpui/src/settings_viewers.rs, app-gpui/src/drill_mail.rs, app-gpui/src/drill_tasks.rs]
read: [docs/decisions/2026-11-11-macos.md]
checks: [clippy, nextest, ci-macos]
manual: docs/manual-tests/MAC-01.md

> Владелец 2026-11-11: приступить к Postman для macOS — Apple Silicon, macOS 13+, подпись ad-hoc
> (`docs/decisions/2026-11-11-macos.md`). Класс B согласован на всю волну, включая CI.

## Цель
Postman собирается на macOS без предупреждений, тесты проходят, CI выдаёт `Postman.app` (zip) и снимки демо-ящика.

## Что входит
- CI — отдельный `.github/workflows/macos.yml` (`ci.yml` не трогается): раннер `macos-15` (arm64), PR и main.
  Job `test`: clippy (`-D warnings`, это и `cargo check`), модули просмотра wasm32, nextest с полной песочницей.
  Job `bundle`: release, `scripts/macos/package.sh` → `Postman.app` (postman, postman-worker, postman-viewer;
  Info.plist: bundle id, версия из `Cargo.toml`, `LSMinimumSystemVersion` 13.0, схема `mailto`; `Postman.icns` из
  `postman.ico`), подпись ad-hoc, `postman-<версия>-macos-arm64.zip`; `--screens` из собранного .app — сцены `list`,
  `letter`, `attachments` (его продолжает PDF), `preview-pdf`, `calendar`, PNG артефактом.
- Ошибки компиляции на macOS: заглушка `notify_with`, `sec_channel_bindings` (только SSPI); блобы КриптоПро
  (`smime/skzi.rs`) — только Windows и тесты; `ldap3` без `gssapi` на macOS (вход Kerberos в каталог — MAC-05).
- Пути данных — одна функция по ОС `postman_core::paths`: `~/Library/Application Support/Postman`, журнал —
  `~/Library/Logs/Postman`, модули администратора — `/Library/Application Support/Postman/viewers`; разбросанные
  `LOCALAPPDATA`/`USERPROFILE` вне Windows-модулей убраны (демо — своя копия правила: он ниже ядра).
- Песочница воркера `crates/worker/src/sandbox/macos.rs`: Seatbelt `sandbox_init` с профилем `(deny default)` без
  исключений — у воркера только уже открытые stdin/stdout; `RLIMIT_CORE` 0, `RLIMIT_NOFILE` 16. Предел памяти и ADR —
  MAC-03.
- Снимок окна для `--screens`: `app-gpui/src/sys/macos.rs` — CoreGraphics (`CGWindowListCreateImage`), окно процесса
  — по `CGWindowListCopyWindowInfo`.

## Приёмка
- `macos.yml` зелёный: clippy, тесты, `.app`, снимки; Windows CI зелёный.
- Ручная проверка `docs/manual-tests/MAC-01.md` на MacBook Air M1 (не блокирует слияние).

## Что не вошло
- Строка `macos-arm64` в выпуске (`ci.yml`, job `release`) — MAC-06.
- Всё системное (Связка ключей, Cmd-клавиши, светофоры, тосты, mailto, журнал в файл, Finder) — MAC-02.
- Все сцены `--screens` и ScreenCaptureKit — MAC-04.

## Зависимости
Нет.
