# BUG-cat-button: «+ категория» открывает меню письма

lane: app-gpui
status: done
class: B
files: [app-gpui/src/menu.rs, app-gpui/src/reading.rs, app-gpui/src/mailer.rs]
checks: [clippy, nextest, screens]
manual: docs/manual-tests/BUG-beta7.md

> Владелец (macOS, 0.1.0-beta.5): «+ категория» в шапке письма открывает контекстное меню (Ответить, Переслать…).

## Причина
Кнопка вызывала `category_key` — всё меню письма у строки списка с подменю категорий.

## Исправление
`category_picker`: только список категорий у кнопки, над открытым письмом; ← и щелчок мимо закрывают.
Сцена `mail-bug-cat-button`.
