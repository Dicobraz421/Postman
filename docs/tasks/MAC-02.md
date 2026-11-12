# MAC-02: базовый клиент на macOS

lane: x
status: done
deps: [MAC-01]
class: B
files: [app-gpui/src/sys/, app-gpui/src/keymap.rs, app-gpui/src/chrome.rs, app-gpui/src/icons.rs, pets: crates/pets-shell/, pets: crates/pets-gpui-macos/ (новый)]
read: [docs/decisions/2026-11-11-macos.md, docs/tasks/MAC-01.md#что-не-вошло]
checks: [clippy, nextest, screens]
manual: docs/manual-tests/MAC-02.md

## Цель
Postman на M1 пригоден для ежедневной почты: вход сохраняется, клавиши и окно — как принято на Mac.

## Что входит
- Вход: пароль — в Связке ключей (`pets-shell/sys/credentials.rs`, крейт `security-framework`) вместо заглушки
  `sys::credentials`.
- Клавиши: Ctrl → Cmd в таблице команд (`ctrl-…` → `cmd-…` по ОС), системные Cmd+Q/H/M/W, Option печатает символы и
  не служит модификатором команд; подсказки и `keys.md` — с ⌘.
- Окно: «светофоры» вместо своих ─ □ ✕ (`chrome.rs`), отступ заголовка; шрифты по ОС (SF Mono/Menlo вместо
  Consolas/Cascadia Mono; значки — встроенные вместо Segoe Fluent Icons).
- Тосты центра уведомлений (`notify`, `notify_with`; нужен .app с bundle id — есть с MAC-01).
- `mailto:` и один экземпляр: `on_open_urls` gpui, unix-сокет вместо именованного канала; «По умолчанию для почты» —
  `LSSetDefaultHandlerForURLScheme` (схема `mailto` в Info.plist — с MAC-01).
- Журнал в файл: `log_to_file` → `~/Library/Logs/Postman` (путь готов — `postman_core::paths::log_dir`).
- Finder: «Открыть», «Показать в Finder» (`open`, `open -R`); метка карантина `com.apple.quarantine` вместо
  Zone.Identifier у сохранённых вложений; в `can_open` запретить `.app .command .pkg .scpt .terminal`.
- Новый общий крейт pets `pets-gpui-macos` (аналог `gpui_windows::pets`): окно, снимок, перетаскивание, тосты —
  снимок из `app-gpui/src/sys/macos.rs` переезжает туда.

## Приёмка
- `docs/manual-tests/MAC-02.md` на MacBook Air M1: вход живого ящика переживает перезапуск, Cmd+N / Cmd+R / Cmd+F,
  тост нового письма, ссылка `mailto:` из Safari открывает новое письмо.
- `macos.yml` зелёный; на Windows снимки затронутых сцен не меняются.

## Что не вошло
- Тексты интерфейса с «Ctrl+…» и «Windows» (`settings-keys-layout-*`, `settings-mailto-hint`, `bulk-pick`,
  `signin-demo`, `settings-reading-aside-1`) — версии для macOS: MAC-04.
- ⌘W в главном окне без закрываемой вкладки ничего не делает (⌘W вкладки глубже системного); скрыть приложение,
  как красная кнопка, — MAC-04.
- Запись сочетания с физическим Control в «Настройки → Клавиатура» на Mac: Control записывается как ⌘ (таблица хранит
  запись Windows) — MAC-04.
- Щелчок по уведомлению открывает Postman, но не письмо — MAC-04.

## Зависимости
MAC-01. Крейт pets — PR в pets, затем ревизия в Postman (полоса deps).
