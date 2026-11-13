# MAC-09: значок в строке меню macOS — аналог значка в трее

lane: x
status: done
deps: [MAC-04, MAC-07]
class: B
files: [app-gpui/src/tray.rs, app-gpui/src/settings_general.rs, app-gpui/src/prefs.rs, app-gpui/res/menubar.png, app-gpui/i18n/ru/app.ftl, app-gpui/i18n/en/app.ftl, app-gpui/i18n/ru/settings.ftl, app-gpui/i18n/en/settings.ftl, crates/core/src/prefs.rs, app-gpui/Cargo.toml, Cargo.toml, Cargo.lock, pets: crates/pets-gpui-macos/]
read: [docs/decisions/2026-11-11-macos.md, docs/tasks/MAC-07.md]
checks: [clippy, nextest, ci-macos]
manual: docs/manual-tests/MAC-09.md

> Волна MAC — класс B, согласована владельцем (`docs/decisions/2026-11-11-macos.md`). Замечание тестера #656
> «macos таскбар». Номер MAC-08 занят (раскладка окна macOS).

## Цель
На Windows у Postman — значок в трее: число непрочитанных в подсказке, меню «Открыть / Выход». На Mac — только
бейдж в Dock; значка в строке меню нет.

## Что входит
- pets: `pets_gpui_macos::status_item::StatusItem` (pets#85) — `NSStatusItem` в системной строке меню, значок-шаблон
  (`isTemplate`: цвет — от темы строки меню), текст рядом, меню из пунктов приложения с обработчиком по номеру.
- `tray.rs`, модуль `mac`: значок «Ленты» (`res/menubar.png`, 36 px — 18 точек вдвое, только прозрачность), рядом —
  число непрочитанных «Входящих» (`tray::menu_bar_title`: 0 — пусто, больше 999 — «999+»), подсказка — `tray::tip`.
  Число обновляется там же, где бейдж Dock. Меню — `tray::menu_bar_menu`: «Открыть Postman», «Новое письмо»
  (`mail.new`), разделитель, «Выйти»; щелчок — канал `Signal` (новый `Signal::Compose`).
- Настройка «Значок в строке меню» (`General::menu_bar_icon`, по умолчанию включена) — «Настройки → Общие» на месте
  «Закрытия окна» (на Mac оно скрыто: закрытие окна всегда прячет приложение). Смена — значок ставится или убирается
  сразу; смена языка — меню заново.
- Ревизия pets — одна у всех pets-крейтов (`app-gpui/Cargo.toml`, `[patch]` корня).

## Приёмка
- Тесты `menu_bar_count_text`, `menu_bar_menu_items` (app-gpui), `menu_bar_icon_defaults_on_and_round_trips`
  (core), `status_item_label_hides_zero` (pets); CI `macOS` зелёный.
- `docs/manual-tests/MAC-09.md` на M1.

## Сделано
Postman — PR «MAC-09: значок в строке меню macOS» (Closes #656); pets#85.

## Зависимости
MAC-04 (канал `Signal` в `tray.rs`), MAC-07 (бейдж Dock).
