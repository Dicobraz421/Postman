# MAC-04: системные возможности macOS

lane: x
status: todo
deps: [MAC-02]
class: B
files: [app-gpui/src/sys/, app-gpui/src/screens.rs, .github/workflows/macos.yml, pets: crates/pets-gpui-macos/, pets: crates/pets-shell/, pets: crates/pets-ui/]
read: [docs/decisions/2026-11-11-macos.md]
checks: [clippy, nextest, screens]
manual: docs/manual-tests/MAC-04.md

## Цель
Всё системное, что есть на Windows, работает и на Mac; снимки всех сцен — в CI macOS.

## Что входит
- Буфер обмена HTML (вставка из Word/Safari): `NSPasteboardTypeHTML` в `pets_shell::clipboard`.
- Перетаскивание вложения в Finder и другие приложения: `NSDraggingSource` / `NSFilePromiseProvider`.
- Автозапуск: `SMAppService.mainApp` (macOS 13+).
- Снимки окна через ScreenCaptureKit вместо устаревшей `CGWindowListCreateImage` (MAC-01); `--screens` — все сцены
  (в MAC-01 — пять: `list`, `letter`, `attachments`, `preview-pdf`, `calendar`), снимки macOS — артефактом `macos.yml`.
- Тема и акцент системы: `NSColor.controlAccentColor`, светлая/тёмная по `NSAppearance`.
- Политики и сведения о машине (`policy`, `upn`, `in_domain`; шифрование диска — FileVault вместо BitLocker):
  `CFPreferences` или заглушки с понятным текстом.
- `--bench` на macOS: память `task_info` (`phys_footprint`), разовый замер простоя CPU на M1.

## Приёмка
- `docs/manual-tests/MAC-04.md` на M1: вставка таблицы из Word, перетаскивание PDF на рабочий стол, автозапуск,
  тёмная тема по системе.
- Все сцены `--screens` проходят на macOS-раннере.

## Зависимости
MAC-02 (крейт `pets-gpui-macos`).
