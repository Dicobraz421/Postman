# MAC-04: системные возможности macOS

lane: x
status: done
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
- Перетаскивание вложения: сделано в MAC-02 (`NSDraggingSource`, копия файла); `NSFilePromiseProvider` (без копии
  заранее) — по замерам.
- Остатки MAC-02: тексты с «Ctrl+…» и «Windows» — варианты для macOS; ⌘W в главном окне без вкладок — скрыть
  приложение; запись физического Control в «Клавиатуре»; щелчок по уведомлению — открыть письмо.
- Автозапуск: `SMAppService.mainApp` (macOS 13+).
- Снимки окна через ScreenCaptureKit вместо устаревшей `CGWindowListCreateImage` (MAC-01); `--screens` — все сцены
  (в MAC-01 — пять: `list`, `letter`, `attachments`, `preview-pdf`, `calendar`), снимки macOS — артефактом `macos.yml`.
- Тема и акцент системы: `NSColor.controlAccentColor`, светлая/тёмная по `NSAppearance`.
- Политики и сведения о машине (`policy`, `upn`, `in_domain`; шифрование диска — FileVault вместо BitLocker):
  `CFPreferences` или заглушки с понятным текстом.
- `--bench` на macOS: память `task_info` (`phys_footprint`), разовый замер простоя CPU на M1.

## Приёмка
- `docs/manual-tests/MAC-04.md` на M1: вставка таблицы из Word, автозапуск,
  тёмная тема по системе.
- Все сцены `--screens` проходят на macOS-раннере.

## Зависимости
MAC-02 (крейт `pets-gpui-macos`).

## Что не вошло
- `NSFilePromiseProvider` (перетаскивание вложения без копии заранее): копия MAC-02 — доли секунды для обычных
  вложений, замеров против неё нет; вернуться, если владелец заметит задержку на больших файлах.
- Вход без пароля в домене (`upn`, `in_domain` — пока «нет», вариант не предлагается) и тексты корпоративного
  (TLS, отзыв, S/MIME, LDAP, «учётная запись Windows») — MAC-05: смысл текста зависит от того, как там сделают
  проверку на macOS.
- Тексты обновления (`update-uac`) — MAC-06 (обновление `.app` на macOS).
- Запуск при входе на macOS — обычным окном, не скрытым (`--minimized` у `SMAppService.mainApp` не передать).
- Папка тем пользователя на macOS не отслеживается: новые файлы тем — со следующего запуска.
- Тесты pets для macOS (`unique_pasteboard_round_trip` на своём буфере, `footprint_is_reported`, щелчок по
  уведомлению) в CI не запускаются: mac-job в CI pets нет — MAC-06.
