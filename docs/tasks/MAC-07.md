# MAC-07: строка меню macOS и бейдж непрочитанных в Dock

lane: x
status: done
deps: [MAC-02, MAC-04]
class: B
files: [app-gpui/src/mac_app.rs, app-gpui/src/mac_menu.rs, app-gpui/src/commands.rs, app-gpui/src/tray.rs, app-gpui/src/sys/mod.rs, app-gpui/src/main.rs, app-gpui/i18n/ru/frame.ftl, app-gpui/i18n/en/frame.ftl, app-gpui/Cargo.toml, Cargo.lock, pets: crates/pets-gpui-macos/]
read: [docs/decisions/2026-11-11-macos.md, docs/tasks/MAC-02.md]
checks: [clippy, nextest, ci-macos]
manual: docs/manual-tests/MAC-07.md

> Волна MAC — класс B, согласована владельцем (`docs/decisions/2026-11-11-macos.md`).

## Цель
Два расхождения Mac с Windows: в строке меню только «Postman» и «Окно» — команд нет; число непрочитанных Windows
показывает в подсказке значка в трее, на Mac — нигде.

## Что входит
- Строка меню (`mac_menu.rs` — таблица на всех ОС, `mac_app.rs` — меню gpui): «Postman» — «Настройки…» ⌘,
  (`settings.open`), «Проверить обновления» (`update.check`), Службы, Скрыть, Выйти; «Файл» — `mail.new`,
  `calendar.new_meeting`, `tab.new`, `tab.close`, `mail.export`; «Правка» — отменить, повторить, вырезать,
  копировать, вставить, выбрать все (действия полей ввода gpui-kit с `OsAction`), `search.focus`; «Вид» —
  `view.mail`/`view.calendar`/`view.contacts`/`view.tasks`, `palette.open`; «Окно» — как было.
  Пункты команд — действия реестра `commands.rs`, клавиши — из той же привязки: `commands::bind` ставит меню заново.
  «О программе» нет: своего экрана нет. Подписи — названия команд или ключи `mac-menu-*` (`frame.ftl`).
- Бейдж в Dock: `pets_gpui_macos::dock` (pets#66), обёртка `sys::set_dock_badge`, вызов — модуль `mac` в
  `tray.rs` при смене непрочитанных «Входящих» (там же, где Windows меняет подсказку). Текст — `tray::badge`:
  0 — без бейджа, 1…999 — число, больше — «999+».
- ревизия pets — одна у всех pets-крейтов `app-gpui/Cargo.toml`.

## Приёмка
- Тесты `mac_menu_items_have_commands_and_texts`, `dock_badge_text`; CI `macOS` зелёный.
- `docs/manual-tests/MAC-07.md` на M1.

## Сделано
PR #615 (выпуск beta.6): меню по таблице команд, бейдж Dock, тесты из «Приёмки».

## Зависимости
MAC-02 (меню приложения), MAC-04 (щелчок по уведомлению в `tray.rs`).
